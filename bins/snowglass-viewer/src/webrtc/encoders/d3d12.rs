//! D3D12 Video Process + Video Encode H.264 backend.
#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    mem::{ManuallyDrop, size_of},
    ptr,
};

use windows::{
    Win32::{
        Foundation::{E_POINTER, HANDLE, RECT},
        Graphics::{Direct3D12::*, Dxgi::Common::*},
        Media::MediaFoundation::*,
        System::Threading::{CreateEventW, INFINITE, WaitForSingleObject},
    },
    core::Interface,
};

use super::super::D3d12Frame;

type Error = Box<dyn std::error::Error + Send + Sync>;

const BITSTREAM_PADDING: u64 = 64 * 1024;

struct H264Support {
    metadata_size: u64,
}

// windows-rs 0.61 predates this binary-compatible extension from current
// DirectX-Headers. Keeping the definition local lets us use feature query 48
// without forcing the rest of the workspace onto a new windows-rs release.
#[repr(C)]
struct H264ResourceRequirements1 {
    node_index: u32,
    codec: D3D12_VIDEO_ENCODER_CODEC,
    profile: D3D12_VIDEO_ENCODER_PROFILE_DESC,
    input_format: DXGI_FORMAT,
    picture_target_resolution: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC,
    is_supported: windows::core::BOOL,
    compressed_bitstream_buffer_access_alignment: u32,
    encoder_metadata_buffer_access_alignment: u32,
    max_encoder_output_metadata_buffer_size: u32,
    optional_metadata: i32,
    codec_configuration: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION,
    qp_map_texture_dimensions: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC,
    satd_map_texture_dimensions: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC,
    bit_allocation_map_texture_dimensions: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC,
    frame_psnr_components: u32,
    subregions_psnr_components: u32,
    subregions_psnr_resolved_metadata_buffer_size: u32,
}

pub(super) struct Encoder {
    _device: ID3D12Device,
    _video_device: ID3D12VideoDevice3,
    processor: ID3D12VideoProcessor,
    process_queue: ID3D12CommandQueue,
    process_allocator: ID3D12CommandAllocator,
    process_list: ID3D12VideoProcessCommandList,
    nv12: ID3D12Resource,
    encoder: ID3D12VideoEncoder,
    encoder_heap: ID3D12VideoEncoderHeap,
    encode_queue: ID3D12CommandQueue,
    encode_allocator: ID3D12CommandAllocator,
    encode_list: ID3D12VideoEncodeCommandList2,
    reconstructed: ID3D12Resource,
    bitstream: ID3D12Resource,
    bitstream_zero: ID3D12Resource,
    bitstream_readback: ID3D12Resource,
    encoded_metadata: ID3D12Resource,
    readback_queue: ID3D12CommandQueue,
    clear_allocator: ID3D12CommandAllocator,
    clear_list: ID3D12GraphicsCommandList,
    readback_allocator: ID3D12CommandAllocator,
    readback_list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    fence_event: HANDLE,
    fence_value: u64,
    width: u32,
    height: u32,
    bitrate: u32,
    frame_rate: u32,
    parameter_sets: Vec<u8>,
    bitstream_offset: u64,
}

impl Encoder {
    pub(super) fn new_for_frame(
        frame: &D3d12Frame,
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: f32,
    ) -> Result<Self, Error> {
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            return Err("D3D12 H.264 encoding requires a non-zero, even-sized frame".into());
        }
        let frame_rate = frame_rate.round().max(1.0) as u32;
        let parameter_sets =
            super::openh264::parameter_sets(width, height, bitrate, frame_rate as f32)?;

        unsafe {
            let device = frame.device.clone();
            let video_device: ID3D12VideoDevice3 = device.cast()?;
            let support = query_h264_support(&video_device, width, height, bitrate, frame_rate)?;
            let bitstream_offset = 0;

            let process_queue = create_queue(&device, D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS)?;
            let process_allocator =
                device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS)?;
            let process_list: ID3D12VideoProcessCommandList = device.CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_VIDEO_PROCESS,
                &process_allocator,
                None,
            )?;
            process_list.Close()?;
            let processor =
                create_processor(&video_device, frame.format, width, height, frame_rate)?;

            let nv12 = create_texture(
                &device,
                width,
                height,
                DXGI_FORMAT_NV12,
                D3D12_RESOURCE_FLAG_NONE,
            )?;
            let reconstructed = create_texture(
                &device,
                width,
                height,
                DXGI_FORMAT_NV12,
                D3D12_RESOURCE_FLAG_NONE,
            )?;

            let mut profile = D3D12_VIDEO_ENCODER_PROFILE_H264_MAIN;
            let profile_desc = profile_desc(&mut profile);
            let mut codec = D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264 {
                ConfigurationFlags: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_FLAG_NONE,
                DirectModeConfig:
                    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_DIRECT_MODES_DISABLED,
                DisableDeblockingFilterConfig:
                    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_SLICES_DEBLOCKING_MODE_0_ALL_LUMA_CHROMA_SLICE_BLOCK_EDGES_ALWAYS_FILTERED,
            };
            let encoder_desc = D3D12_VIDEO_ENCODER_DESC {
                NodeMask: 0,
                Flags: D3D12_VIDEO_ENCODER_FLAG_NONE,
                EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_H264,
                EncodeProfile: profile_desc,
                InputFormat: DXGI_FORMAT_NV12,
                CodecConfiguration: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION {
                    DataSize: size_of::<D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264>() as u32,
                    Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_0 {
                        pH264Config: &raw mut codec,
                    },
                },
                MaxMotionEstimationPrecision:
                    D3D12_VIDEO_ENCODER_MOTION_ESTIMATION_PRECISION_MODE_MAXIMUM,
            };
            let encoder =
                video_device.CreateVideoEncoder::<ID3D12VideoEncoder>(&raw const encoder_desc)?;

            let resolution = D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
                Width: width,
                Height: height,
            };
            let mut level = D3D12_VIDEO_ENCODER_LEVELS_H264_41;
            let heap_desc = D3D12_VIDEO_ENCODER_HEAP_DESC {
                NodeMask: 0,
                Flags: D3D12_VIDEO_ENCODER_HEAP_FLAG_NONE,
                EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_H264,
                EncodeProfile: profile_desc,
                EncodeLevel: D3D12_VIDEO_ENCODER_LEVEL_SETTING {
                    DataSize: size_of::<D3D12_VIDEO_ENCODER_LEVELS_H264>() as u32,
                    Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 {
                        pH264LevelSetting: &raw mut level,
                    },
                },
                ResolutionsListCount: 1,
                pResolutionList: &raw const resolution,
            };
            let encoder_heap = video_device
                .CreateVideoEncoderHeap::<ID3D12VideoEncoderHeap>(&raw const heap_desc)?;

            let encode_queue = create_queue(&device, D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE)?;
            let encode_allocator =
                device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE)?;
            let encode_list: ID3D12VideoEncodeCommandList2 = device.CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_VIDEO_ENCODE,
                &encode_allocator,
                None,
            )?;
            encode_list.Close()?;

            let bitstream_size =
                bitstream_offset + (width as u64 * height as u64 * 3) + BITSTREAM_PADDING;
            let bitstream = create_buffer(
                &device,
                bitstream_size,
                D3D12_HEAP_TYPE_DEFAULT,
                D3D12_RESOURCE_STATE_COMMON,
            )?;
            let bitstream_readback = create_buffer(
                &device,
                bitstream_size,
                D3D12_HEAP_TYPE_READBACK,
                D3D12_RESOURCE_STATE_COPY_DEST,
            )?;
            let bitstream_zero = create_buffer(
                &device,
                bitstream_size,
                D3D12_HEAP_TYPE_UPLOAD,
                D3D12_RESOURCE_STATE_GENERIC_READ,
            )?;
            let mut zero_mapping = ptr::null_mut();
            bitstream_zero.Map(0, None, Some(&mut zero_mapping))?;
            ptr::write_bytes(zero_mapping.cast::<u8>(), 0, bitstream_size as usize);
            bitstream_zero.Unmap(0, None);
            let encoded_metadata = create_buffer(
                &device,
                support.metadata_size,
                D3D12_HEAP_TYPE_DEFAULT,
                D3D12_RESOURCE_STATE_COMMON,
            )?;
            let readback_queue = create_queue(&device, D3D12_COMMAND_LIST_TYPE_COPY)?;
            let clear_allocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COPY)?;
            let clear_list: ID3D12GraphicsCommandList = device.CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_COPY,
                &clear_allocator,
                None,
            )?;
            clear_list.Close()?;
            let readback_allocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_COPY)?;
            let readback_list: ID3D12GraphicsCommandList = device.CreateCommandList(
                0,
                D3D12_COMMAND_LIST_TYPE_COPY,
                &readback_allocator,
                None,
            )?;
            readback_list.Close()?;
            let fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let fence_event = CreateEventW(None, false, false, None)?;

            Ok(Self {
                _device: device,
                _video_device: video_device,
                processor,
                process_queue,
                process_allocator,
                process_list,
                nv12,
                encoder,
                encoder_heap,
                encode_queue,
                encode_allocator,
                encode_list,
                reconstructed,
                bitstream,
                bitstream_zero,
                bitstream_readback,
                encoded_metadata,
                readback_queue,
                clear_allocator,
                clear_list,
                readback_allocator,
                readback_list,
                fence,
                fence_event,
                fence_value: 0,
                width,
                height,
                bitrate,
                frame_rate,
                parameter_sets,
                bitstream_offset,
            })
        }
    }

    pub(super) fn encode_frame(
        &mut self,
        frame: &mut D3d12Frame,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, Error> {
        if width != self.width || height != self.height {
            return Err("D3D12 encoder resolution changed".into());
        }
        unsafe {
            self.submit_bitstream_clear()
                .map_err(|error| format!("D3D12 bitstream clear failed: {error}"))?;
            self.submit_conversion(frame)
                .map_err(|error| format!("D3D12 video conversion failed: {error}"))?;
            self.submit_encode()
                .map_err(|error| format!("D3D12 video encode submission failed: {error}"))?;
            self.submit_readback()
                .map_err(|error| format!("D3D12 compressed readback submission failed: {error}"))?;
            self.read_bitstream()
                .map_err(|error| format!("D3D12 compressed readback failed: {error}").into())
        }
    }

    unsafe fn submit_bitstream_clear(&mut self) -> Result<(), Error> {
        unsafe {
            self.clear_allocator.Reset()?;
            self.clear_list.Reset(&self.clear_allocator, None)?;
            graphics_transition(
                &self.clear_list,
                &self.bitstream,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_DEST,
            );
            self.clear_list.CopyBufferRegion(
                &self.bitstream,
                0,
                &self.bitstream_zero,
                0,
                self.bitstream.GetDesc().Width,
            );
            graphics_transition(
                &self.clear_list,
                &self.bitstream,
                D3D12_RESOURCE_STATE_COPY_DEST,
                D3D12_RESOURCE_STATE_COMMON,
            );
            self.clear_list.Close()?;
            let list: ID3D12CommandList = self.clear_list.cast()?;
            self.readback_queue.ExecuteCommandLists(&[Some(list)]);
            self.fence_value += 1;
            self.readback_queue.Signal(&self.fence, self.fence_value)?;
            Ok(())
        }
    }

    unsafe fn submit_conversion(&mut self, frame: &mut D3d12Frame) -> Result<(), Error> {
        unsafe {
            self.process_allocator.Reset()?;
            self.process_list.Reset(&self.process_allocator)?;
            self.process_queue
                .Wait(&frame.ready_fence, frame.ready_value)?;
            process_transition(
                &self.process_list,
                &frame.resource,
                frame.state,
                D3D12_RESOURCE_STATE_VIDEO_PROCESS_READ,
            );
            process_transition(
                &self.process_list,
                &self.nv12,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_VIDEO_PROCESS_WRITE,
            );

            let rectangle = RECT {
                left: 0,
                top: 0,
                right: self.width as i32,
                bottom: self.height as i32,
            };
            let mut input = D3D12_VIDEO_PROCESS_INPUT_STREAM_ARGUMENTS::default();
            input.InputStream[0].pTexture2D = ManuallyDrop::new(Some(frame.resource.clone()));
            input.Transform = D3D12_VIDEO_PROCESS_TRANSFORM {
                SourceRectangle: rectangle,
                DestinationRectangle: rectangle,
                Orientation: D3D12_VIDEO_PROCESS_ORIENTATION_DEFAULT,
            };
            input.Flags = D3D12_VIDEO_PROCESS_INPUT_STREAM_FLAG_NONE;
            let mut output = D3D12_VIDEO_PROCESS_OUTPUT_STREAM_ARGUMENTS::default();
            output.OutputStream[0].pTexture2D = ManuallyDrop::new(Some(self.nv12.clone()));
            output.TargetRectangle = rectangle;
            self.process_list.ProcessFrames(
                &self.processor,
                &raw const output,
                std::slice::from_ref(&input),
            );
            ManuallyDrop::drop(&mut input.InputStream[0].pTexture2D);
            ManuallyDrop::drop(&mut output.OutputStream[0].pTexture2D);

            process_transition(
                &self.process_list,
                &frame.resource,
                D3D12_RESOURCE_STATE_VIDEO_PROCESS_READ,
                frame.state,
            );
            process_transition(
                &self.process_list,
                &self.nv12,
                D3D12_RESOURCE_STATE_VIDEO_PROCESS_WRITE,
                D3D12_RESOURCE_STATE_COMMON,
            );
            self.process_list.Close()?;
            let list: ID3D12CommandList = self.process_list.cast()?;
            self.process_queue.ExecuteCommandLists(&[Some(list)]);
            self.fence_value += 1;
            self.process_queue.Signal(&self.fence, self.fence_value)?;
            self.process_queue
                .Signal(&frame.completion_fence, frame.completion_value)?;
            frame.mark_completion_submitted();
            Ok(())
        }
    }

    unsafe fn submit_encode(&mut self) -> Result<(), Error> {
        unsafe {
            self.encode_allocator.Reset()?;
            self.encode_list.Reset(&self.encode_allocator)?;
            self.encode_queue.Wait(&self.fence, self.fence_value)?;

            encode_transition(
                &self.encode_list,
                &self.nv12,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ,
            );
            for resource in [&self.reconstructed, &self.bitstream, &self.encoded_metadata] {
                encode_transition(
                    &self.encode_list,
                    resource,
                    D3D12_RESOURCE_STATE_COMMON,
                    D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE,
                );
            }

            let cbr = D3D12_VIDEO_ENCODER_RATE_CONTROL_CBR {
                TargetBitRate: self.bitrate as u64,
                ..Default::default()
            };
            let rate_control = D3D12_VIDEO_ENCODER_RATE_CONTROL {
                Mode: D3D12_VIDEO_ENCODER_RATE_CONTROL_MODE_CBR,
                Flags: D3D12_VIDEO_ENCODER_RATE_CONTROL_FLAG_NONE,
                ConfigParams: D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS {
                    DataSize: size_of::<D3D12_VIDEO_ENCODER_RATE_CONTROL_CBR>() as u32,
                    Anonymous: D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS_0 {
                        pConfiguration_CBR: &raw const cbr,
                    },
                },
                TargetFrameRate: DXGI_RATIONAL {
                    Numerator: self.frame_rate,
                    Denominator: 1,
                },
            };
            let mut gop = D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264 {
                GOPLength: 1,
                PPicturePeriod: 0,
                pic_order_cnt_type: 2,
                log2_max_frame_num_minus4: 11,
                log2_max_pic_order_cnt_lsb_minus4: 0,
            };
            let sequence = D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_DESC {
                Flags: D3D12_VIDEO_ENCODER_SEQUENCE_CONTROL_FLAG_NONE,
                RateControl: rate_control,
                PictureTargetResolution: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
                    Width: self.width,
                    Height: self.height,
                },
                SelectedLayoutMode: D3D12_VIDEO_ENCODER_FRAME_SUBREGION_LAYOUT_MODE_FULL_FRAME,
                CodecGopSequence: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE {
                    DataSize: size_of::<D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264>() as u32,
                    Anonymous: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_0 {
                        pH264GroupOfPictures: &raw mut gop,
                    },
                },
                ..Default::default()
            };
            let mut h264_picture = D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264 {
                Flags: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264_FLAG_NONE,
                FrameType: D3D12_VIDEO_ENCODER_FRAME_TYPE_H264_IDR_FRAME,
                ..Default::default()
            };
            let picture = D3D12_VIDEO_ENCODER_PICTURE_CONTROL_DESC {
                Flags: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_FLAG_USED_AS_REFERENCE_PICTURE,
                PictureControlCodecData: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA {
                    DataSize: size_of::<D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_H264>()
                        as u32,
                    Anonymous: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_0 {
                        pH264PicData: &raw mut h264_picture,
                    },
                },
                ..Default::default()
            };
            let mut input = D3D12_VIDEO_ENCODER_ENCODEFRAME_INPUT_ARGUMENTS {
                SequenceControlDesc: sequence,
                PictureControlDesc: picture,
                pInputFrame: ManuallyDrop::new(Some(self.nv12.clone())),
                InputFrameSubresource: 0,
                CurrentFrameBitstreamMetadataSize: 0,
            };
            let mut output = D3D12_VIDEO_ENCODER_ENCODEFRAME_OUTPUT_ARGUMENTS {
                Bitstream: D3D12_VIDEO_ENCODER_COMPRESSED_BITSTREAM {
                    pBuffer: ManuallyDrop::new(Some(self.bitstream.clone())),
                    FrameStartOffset: self.bitstream_offset,
                },
                ReconstructedPicture: D3D12_VIDEO_ENCODER_RECONSTRUCTED_PICTURE {
                    pReconstructedPicture: ManuallyDrop::new(Some(self.reconstructed.clone())),
                    ReconstructedPictureSubresource: 0,
                },
                EncoderOutputMetadata: D3D12_VIDEO_ENCODER_ENCODE_OPERATION_METADATA_BUFFER {
                    pBuffer: ManuallyDrop::new(Some(self.encoded_metadata.clone())),
                    Offset: 0,
                },
            };
            self.encode_list.EncodeFrame(
                &self.encoder,
                &self.encoder_heap,
                &raw const input,
                &raw const output,
            );

            encode_transition(
                &self.encode_list,
                &self.nv12,
                D3D12_RESOURCE_STATE_VIDEO_ENCODE_READ,
                D3D12_RESOURCE_STATE_COMMON,
            );
            encode_transition(
                &self.encode_list,
                &self.reconstructed,
                D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE,
                D3D12_RESOURCE_STATE_COMMON,
            );
            encode_transition(
                &self.encode_list,
                &self.bitstream,
                D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE,
                D3D12_RESOURCE_STATE_COMMON,
            );
            encode_transition(
                &self.encode_list,
                &self.encoded_metadata,
                D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE,
                D3D12_RESOURCE_STATE_COMMON,
            );
            self.encode_list.Close()?;

            ManuallyDrop::drop(&mut input.pInputFrame);
            ManuallyDrop::drop(&mut output.Bitstream.pBuffer);
            ManuallyDrop::drop(&mut output.ReconstructedPicture.pReconstructedPicture);
            ManuallyDrop::drop(&mut output.EncoderOutputMetadata.pBuffer);
            let list: ID3D12CommandList = self.encode_list.cast()?;
            self.encode_queue.ExecuteCommandLists(&[Some(list)]);
            self.fence_value += 1;
            self.encode_queue.Signal(&self.fence, self.fence_value)?;
            Ok(())
        }
    }

    unsafe fn submit_readback(&mut self) -> Result<(), Error> {
        unsafe {
            self.readback_allocator.Reset()?;
            self.readback_list.Reset(&self.readback_allocator, None)?;
            self.readback_queue.Wait(&self.fence, self.fence_value)?;
            self.readback_list.CopyBufferRegion(
                &self.bitstream_readback,
                0,
                &self.bitstream,
                0,
                self.bitstream.GetDesc().Width,
            );
            self.readback_list.Close()?;
            let list: ID3D12CommandList = self.readback_list.cast()?;
            self.readback_queue.ExecuteCommandLists(&[Some(list)]);
            self.fence_value += 1;
            self.readback_queue.Signal(&self.fence, self.fence_value)?;
            Ok(())
        }
    }

    unsafe fn read_bitstream(&self) -> Result<Vec<u8>, Error> {
        unsafe {
            wait_fence(&self.fence, self.fence_value, self.fence_event)?;
            let desc = self.bitstream_readback.GetDesc();
            let mut mapped = ptr::null_mut();
            self.bitstream_readback.Map(0, None, Some(&mut mapped))?;
            let output = std::slice::from_raw_parts(
                mapped.cast::<u8>().add(self.bitstream_offset as usize),
                (desc.Width - self.bitstream_offset) as usize,
            );
            let encoded_len = output
                .iter()
                .rposition(|byte| *byte != 0)
                .map(|index| index + 1)
                .unwrap_or(0);
            if encoded_len == 0 {
                self.bitstream_readback.Unmap(0, None);
                return Err("D3D12 encoder returned an empty bitstream".into());
            }
            let driver = &output[..encoded_len];
            let mut result = self.parameter_sets.clone();
            if !driver.starts_with(&[0, 0, 1]) && !driver.starts_with(&[0, 0, 0, 1]) {
                result.extend_from_slice(&[0, 0, 0, 1]);
            }
            result.extend_from_slice(driver);
            self.bitstream_readback.Unmap(0, None);
            Ok(result)
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        let _ = unsafe { wait_fence(&self.fence, self.fence_value, self.fence_event) };
        if !self.fence_event.is_invalid() {
            let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.fence_event) };
        }
    }
}

pub(super) fn readback_rgba(
    frame: &mut D3d12Frame,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, Error> {
    unsafe {
        let device = frame.device.clone();
        let desc = frame.resource.GetDesc();
        let mut footprint = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
        let mut total = 0;
        device.GetCopyableFootprints(
            &raw const desc,
            0,
            1,
            0,
            Some(&raw mut footprint),
            None,
            None,
            Some(&raw mut total),
        );
        let readback = create_buffer(
            &device,
            total,
            D3D12_HEAP_TYPE_READBACK,
            D3D12_RESOURCE_STATE_COPY_DEST,
        )?;
        let queue = create_queue(&device, D3D12_COMMAND_LIST_TYPE_DIRECT)?;
        let allocator: ID3D12CommandAllocator =
            device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
        let list: ID3D12GraphicsCommandList =
            device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &allocator, None)?;
        queue.Wait(&frame.ready_fence, frame.ready_value)?;
        graphics_transition(
            &list,
            &frame.resource,
            frame.state,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        );
        let mut destination = D3D12_TEXTURE_COPY_LOCATION {
            pResource: ManuallyDrop::new(Some(readback.clone())),
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: footprint,
            },
        };
        let mut source = D3D12_TEXTURE_COPY_LOCATION {
            pResource: ManuallyDrop::new(Some(frame.resource.clone())),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                SubresourceIndex: 0,
            },
        };
        list.CopyTextureRegion(&destination, 0, 0, 0, &source, None);
        ManuallyDrop::drop(&mut destination.pResource);
        ManuallyDrop::drop(&mut source.pResource);
        graphics_transition(
            &list,
            &frame.resource,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
            frame.state,
        );
        list.Close()?;
        let command: ID3D12CommandList = list.cast()?;
        queue.ExecuteCommandLists(&[Some(command)]);
        queue.Signal(&frame.completion_fence, frame.completion_value)?;
        frame.mark_completion_submitted();
        let fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
        let event = CreateEventW(None, false, false, None)?;
        queue.Signal(&fence, 1)?;
        wait_fence(&fence, 1, event)?;
        windows::Win32::Foundation::CloseHandle(event)?;

        let mut mapped = ptr::null_mut();
        readback.Map(0, None, Some(&mut mapped))?;
        let bytes = std::slice::from_raw_parts(mapped.cast::<u8>(), total as usize);
        let rgba = pixels_to_rgba(
            frame.format,
            bytes,
            width as usize,
            height as usize,
            footprint.Footprint.RowPitch as usize,
        )?;
        readback.Unmap(0, None);
        Ok(rgba)
    }
}

unsafe fn query_h264_support(
    device: &ID3D12VideoDevice3,
    width: u32,
    height: u32,
    bitrate: u32,
    frame_rate: u32,
) -> Result<H264Support, Error> {
    unsafe {
        let mut codec_support = D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC {
            NodeIndex: 0,
            Codec: D3D12_VIDEO_ENCODER_CODEC_H264,
            ..Default::default()
        };
        check_video_feature(
            device,
            D3D12_FEATURE_VIDEO_ENCODER_CODEC,
            &mut codec_support,
        )?;
        if !codec_support.IsSupported.as_bool() {
            return Err("adapter does not support D3D12 H.264 video encoding".into());
        }

        let mut profile = D3D12_VIDEO_ENCODER_PROFILE_H264_MAIN;
        let mut min_level = D3D12_VIDEO_ENCODER_LEVELS_H264_1;
        let mut max_level = D3D12_VIDEO_ENCODER_LEVELS_H264_1;
        let mut profile_support = D3D12_FEATURE_DATA_VIDEO_ENCODER_PROFILE_LEVEL {
            NodeIndex: 0,
            Codec: D3D12_VIDEO_ENCODER_CODEC_H264,
            Profile: profile_desc(&mut profile),
            MinSupportedLevel: level_desc(&mut min_level),
            MaxSupportedLevel: level_desc(&mut max_level),
            ..Default::default()
        };
        check_video_feature(
            device,
            D3D12_FEATURE_VIDEO_ENCODER_PROFILE_LEVEL,
            &mut profile_support,
        )?;
        if !profile_support.IsSupported.as_bool() {
            return Err("adapter does not support the H.264 Main profile".into());
        }

        let mut input_support = D3D12_FEATURE_DATA_VIDEO_ENCODER_INPUT_FORMAT {
            NodeIndex: 0,
            Codec: D3D12_VIDEO_ENCODER_CODEC_H264,
            Profile: profile_desc(&mut profile),
            Format: DXGI_FORMAT_NV12,
            ..Default::default()
        };
        check_video_feature(
            device,
            D3D12_FEATURE_VIDEO_ENCODER_INPUT_FORMAT,
            &mut input_support,
        )?;
        if !input_support.IsSupported.as_bool() {
            return Err("adapter does not support NV12 H.264 Main video encoding".into());
        }

        let mut ratio_count = D3D12_FEATURE_DATA_VIDEO_ENCODER_OUTPUT_RESOLUTION_RATIOS_COUNT {
            NodeIndex: 0,
            Codec: D3D12_VIDEO_ENCODER_CODEC_H264,
            ..Default::default()
        };
        check_video_feature(
            device,
            D3D12_FEATURE_VIDEO_ENCODER_OUTPUT_RESOLUTION_RATIOS_COUNT,
            &mut ratio_count,
        )?;
        let mut ratios = vec![
            D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_RATIO_DESC::default();
            ratio_count.ResolutionRatiosCount as usize
        ];
        let mut resolution_support = D3D12_FEATURE_DATA_VIDEO_ENCODER_OUTPUT_RESOLUTION {
            NodeIndex: 0,
            Codec: D3D12_VIDEO_ENCODER_CODEC_H264,
            ResolutionRatiosCount: ratio_count.ResolutionRatiosCount,
            pResolutionRatios: if ratios.is_empty() {
                ptr::null_mut()
            } else {
                ratios.as_mut_ptr()
            },
            ..Default::default()
        };
        check_video_feature(
            device,
            D3D12_FEATURE_VIDEO_ENCODER_OUTPUT_RESOLUTION,
            &mut resolution_support,
        )
        .map_err(|error| {
            format!(
                "{error} (driver reported {} resolution ratios)",
                ratio_count.ResolutionRatiosCount
            )
        })?;
        let width_multiple = resolution_support.ResolutionWidthMultipleRequirement.max(1);
        let height_multiple = resolution_support
            .ResolutionHeightMultipleRequirement
            .max(1);
        if !resolution_support.IsSupported.as_bool()
            || width < resolution_support.MinResolutionSupported.Width
            || height < resolution_support.MinResolutionSupported.Height
            || width > resolution_support.MaxResolutionSupported.Width
            || height > resolution_support.MaxResolutionSupported.Height
            || !width.is_multiple_of(width_multiple)
            || !height.is_multiple_of(height_multiple)
        {
            return Err(
                format!("adapter does not support H.264 encoding at {width}x{height}").into(),
            );
        }

        let mut requirements = D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOURCE_REQUIREMENTS {
            NodeIndex: 0,
            Codec: D3D12_VIDEO_ENCODER_CODEC_H264,
            Profile: profile_desc(&mut profile),
            InputFormat: DXGI_FORMAT_NV12,
            PictureTargetResolution: D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
                Width: width,
                Height: height,
            },
            ..Default::default()
        };
        check_video_feature(
            device,
            D3D12_FEATURE_VIDEO_ENCODER_RESOURCE_REQUIREMENTS,
            &mut requirements,
        )?;
        if !requirements.IsSupported.as_bool() {
            return Err("adapter rejected the D3D12 H.264 encoder resource requirements".into());
        }

        // The combined query is required in addition to the individual capability
        // queries: it validates that this exact codec, GOP, rate control and
        // resolution combination can be used in one encoding session.
        let mut codec_configuration = D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264 {
            ConfigurationFlags: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_FLAG_NONE,
            DirectModeConfig:
                D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_DIRECT_MODES_DISABLED,
            DisableDeblockingFilterConfig:
                D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_SLICES_DEBLOCKING_MODE_0_ALL_LUMA_CHROMA_SLICE_BLOCK_EDGES_ALWAYS_FILTERED,
        };
        let mut gop = D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264 {
            GOPLength: 1,
            PPicturePeriod: 0,
            pic_order_cnt_type: 2,
            log2_max_frame_num_minus4: 11,
            log2_max_pic_order_cnt_lsb_minus4: 0,
        };
        let cbr = D3D12_VIDEO_ENCODER_RATE_CONTROL_CBR {
            TargetBitRate: bitrate as u64,
            ..Default::default()
        };
        let resolution = D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
            Width: width,
            Height: height,
        };
        let mut resolution_limits =
            D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOLUTION_SUPPORT_LIMITS::default();
        let mut suggested_profile = D3D12_VIDEO_ENCODER_PROFILE_H264_MAIN;
        let mut suggested_level = D3D12_VIDEO_ENCODER_LEVELS_H264_41;
        let mut session_support = D3D12_FEATURE_DATA_VIDEO_ENCODER_SUPPORT1 {
            NodeIndex: 0,
            Codec: D3D12_VIDEO_ENCODER_CODEC_H264,
            InputFormat: DXGI_FORMAT_NV12,
            CodecConfiguration: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION {
                DataSize: size_of::<D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264>() as u32,
                Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_0 {
                    pH264Config: &raw mut codec_configuration,
                },
            },
            CodecGopSequence: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE {
                DataSize: size_of::<D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_H264>() as u32,
                Anonymous: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_0 {
                    pH264GroupOfPictures: &raw mut gop,
                },
            },
            RateControl: D3D12_VIDEO_ENCODER_RATE_CONTROL {
                Mode: D3D12_VIDEO_ENCODER_RATE_CONTROL_MODE_CBR,
                Flags: D3D12_VIDEO_ENCODER_RATE_CONTROL_FLAG_NONE,
                ConfigParams: D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS {
                    DataSize: size_of::<D3D12_VIDEO_ENCODER_RATE_CONTROL_CBR>() as u32,
                    Anonymous: D3D12_VIDEO_ENCODER_RATE_CONTROL_CONFIGURATION_PARAMS_0 {
                        pConfiguration_CBR: &raw const cbr,
                    },
                },
                TargetFrameRate: DXGI_RATIONAL {
                    Numerator: frame_rate,
                    Denominator: 1,
                },
            },
            IntraRefresh: D3D12_VIDEO_ENCODER_INTRA_REFRESH_MODE_NONE,
            SubregionFrameEncoding: D3D12_VIDEO_ENCODER_FRAME_SUBREGION_LAYOUT_MODE_FULL_FRAME,
            ResolutionsListCount: 1,
            pResolutionList: &raw const resolution,
            MaxReferenceFramesInDPB: 0,
            SuggestedProfile: profile_desc(&mut suggested_profile),
            SuggestedLevel: level_desc(&mut suggested_level),
            pResolutionDependentSupport: &raw mut resolution_limits,
            ..Default::default()
        };
        check_video_feature(
            device,
            D3D12_FEATURE_VIDEO_ENCODER_SUPPORT1,
            &mut session_support,
        )?;
        if session_support.SupportFlags.0 & D3D12_VIDEO_ENCODER_SUPPORT_FLAG_GENERAL_SUPPORT_OK.0
            == 0
        {
            return Err(format!(
                "adapter rejected D3D12 H.264 session (validation flags {:#x})",
                session_support.ValidationFlags.0
            )
            .into());
        }
        let mut requirements1: H264ResourceRequirements1 = std::mem::zeroed();
        requirements1.codec = D3D12_VIDEO_ENCODER_CODEC_H264;
        requirements1.profile = profile_desc(&mut profile);
        requirements1.input_format = DXGI_FORMAT_NV12;
        requirements1.picture_target_resolution = resolution;
        requirements1.codec_configuration = D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION {
            DataSize: size_of::<D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264>() as u32,
            Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_0 {
                pH264Config: &raw mut codec_configuration,
            },
        };
        check_video_feature(device, D3D12_FEATURE_VIDEO(48), &mut requirements1)?;
        if !requirements1.is_supported.as_bool() {
            return Err("adapter rejected the extended D3D12 H.264 resource requirements".into());
        }

        Ok(H264Support {
            metadata_size: align_up(
                requirements1.max_encoder_output_metadata_buffer_size as u64,
                requirements1
                    .encoder_metadata_buffer_access_alignment
                    .max(1) as u64,
            ),
        })
    }
}

unsafe fn check_video_feature<T>(
    device: &ID3D12VideoDevice3,
    feature: D3D12_FEATURE_VIDEO,
    data: &mut T,
) -> Result<(), Error> {
    unsafe {
        device
            .CheckFeatureSupport(
                feature,
                (data as *mut T).cast::<c_void>(),
                size_of::<T>() as u32,
            )
            .map_err(|error| {
                format!("D3D12 video feature query {} failed: {error}", feature.0).into()
            })
    }
}

unsafe fn create_processor(
    device: &ID3D12VideoDevice3,
    input_format: DXGI_FORMAT,
    width: u32,
    height: u32,
    frame_rate: u32,
) -> Result<ID3D12VideoProcessor, Error> {
    let ratio = DXGI_RATIONAL {
        Numerator: width,
        Denominator: height,
    };
    let rate = DXGI_RATIONAL {
        Numerator: frame_rate,
        Denominator: 1,
    };
    let size = D3D12_VIDEO_SIZE_RANGE {
        MaxWidth: width,
        MaxHeight: height,
        MinWidth: width,
        MinHeight: height,
    };
    let input = D3D12_VIDEO_PROCESS_INPUT_STREAM_DESC {
        Format: input_format,
        ColorSpace: DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
        SourceAspectRatio: ratio,
        DestinationAspectRatio: ratio,
        FrameRate: rate,
        SourceSizeRange: size,
        DestinationSizeRange: size,
        StereoFormat: D3D12_VIDEO_FRAME_STEREO_FORMAT_NONE,
        FieldType: D3D12_VIDEO_FIELD_TYPE_NONE,
        DeinterlaceMode: D3D12_VIDEO_PROCESS_DEINTERLACE_FLAG_NONE,
        FilterFlags: D3D12_VIDEO_PROCESS_FILTER_FLAG_NONE,
        ..Default::default()
    };
    let output = D3D12_VIDEO_PROCESS_OUTPUT_STREAM_DESC {
        Format: DXGI_FORMAT_NV12,
        ColorSpace: DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
        AlphaFillMode: D3D12_VIDEO_PROCESS_ALPHA_FILL_MODE_OPAQUE,
        FrameRate: rate,
        ..Default::default()
    };
    Ok(unsafe { device.CreateVideoProcessor(0, &raw const output, &[input])? })
}

fn profile_desc(
    profile: &mut D3D12_VIDEO_ENCODER_PROFILE_H264,
) -> D3D12_VIDEO_ENCODER_PROFILE_DESC {
    D3D12_VIDEO_ENCODER_PROFILE_DESC {
        DataSize: size_of::<D3D12_VIDEO_ENCODER_PROFILE_H264>() as u32,
        Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 {
            pH264Profile: profile,
        },
    }
}

fn level_desc(level: &mut D3D12_VIDEO_ENCODER_LEVELS_H264) -> D3D12_VIDEO_ENCODER_LEVEL_SETTING {
    D3D12_VIDEO_ENCODER_LEVEL_SETTING {
        DataSize: size_of::<D3D12_VIDEO_ENCODER_LEVELS_H264>() as u32,
        Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 {
            pH264LevelSetting: level,
        },
    }
}

unsafe fn create_queue(
    device: &ID3D12Device,
    kind: D3D12_COMMAND_LIST_TYPE,
) -> windows::core::Result<ID3D12CommandQueue> {
    unsafe {
        device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
            Type: kind,
            Priority: 0,
            Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
            NodeMask: 0,
        })
    }
}

unsafe fn create_texture(
    device: &ID3D12Device,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    flags: D3D12_RESOURCE_FLAGS,
) -> windows::core::Result<ID3D12Resource> {
    let mut resource = None;
    unsafe {
        device.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                ..Default::default()
            },
            D3D12_HEAP_FLAG_NONE,
            &D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Width: width as u64,
                Height: height,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                Flags: flags,
                ..Default::default()
            },
            D3D12_RESOURCE_STATE_COMMON,
            None,
            &mut resource,
        )?;
    }
    resource.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))
}

unsafe fn create_buffer(
    device: &ID3D12Device,
    size: u64,
    heap_type: D3D12_HEAP_TYPE,
    state: D3D12_RESOURCE_STATES,
) -> windows::core::Result<ID3D12Resource> {
    let mut resource = None;
    unsafe {
        device.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES {
                Type: heap_type,
                ..Default::default()
            },
            D3D12_HEAP_FLAG_NONE,
            &D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: size.max(1),
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                ..Default::default()
            },
            state,
            None,
            &mut resource,
        )?;
    }
    resource.ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))
}

unsafe fn wait_fence(fence: &ID3D12Fence, value: u64, event: HANDLE) -> windows::core::Result<()> {
    unsafe {
        if value != 0 && fence.GetCompletedValue() < value {
            fence.SetEventOnCompletion(value, event)?;
            WaitForSingleObject(event, INFINITE);
        }
    }
    Ok(())
}

fn pixels_to_rgba(
    format: DXGI_FORMAT,
    packed: &[u8],
    width: usize,
    height: usize,
    row_pitch: usize,
) -> Result<Vec<u8>, Error> {
    if !matches!(
        format,
        DXGI_FORMAT_R8G8B8A8_UNORM
            | DXGI_FORMAT_R8G8B8A8_UNORM_SRGB
            | DXGI_FORMAT_B8G8R8A8_UNORM
            | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB
            | DXGI_FORMAT_R10G10B10A2_UNORM
    ) {
        return Err(format!("OpenH264 fallback does not support DXGI format {format:?}").into());
    }
    let mut output = Vec::with_capacity(width * height * 4);
    for row in packed.chunks_exact(row_pitch).take(height) {
        for bytes in row[..width * 4].chunks_exact(4) {
            if format == DXGI_FORMAT_R10G10B10A2_UNORM {
                let pixel = u32::from_le_bytes(bytes.try_into().expect("four-byte pixel"));
                output.extend_from_slice(&[
                    ((pixel & 0x3ff) * 255 / 1023) as u8,
                    (((pixel >> 10) & 0x3ff) * 255 / 1023) as u8,
                    (((pixel >> 20) & 0x3ff) * 255 / 1023) as u8,
                    255,
                ]);
            } else if matches!(
                format,
                DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB
            ) {
                output.extend_from_slice(&[bytes[2], bytes[1], bytes[0], bytes[3]]);
            } else {
                output.extend_from_slice(bytes);
            }
        }
    }
    Ok(output)
}

fn align_up(value: u64, alignment: u64) -> u64 {
    value.div_ceil(alignment) * alignment
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::{
        Direct3D::D3D_FEATURE_LEVEL_11_0,
        Direct3D12::{D3D12CreateDevice, D3D12GetDebugInterface, ID3D12Debug},
        Dxgi::{
            CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE,
            IDXGIAdapter1, IDXGIFactory6,
        },
    };

    #[test]
    #[ignore = "requires a D3D12 adapter with H.264 video encoding"]
    fn hardware_encode_smoke_test() {
        unsafe {
            let mut debug = None;
            if D3D12GetDebugInterface::<ID3D12Debug>(&mut debug).is_ok()
                && let Some(debug) = debug
            {
                debug.EnableDebugLayer();
            }
            let factory: IDXGIFactory6 =
                CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)).expect("create DXGI factory");
            let adapter: IDXGIAdapter1 = factory
                .EnumAdapterByGpuPreference(0, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE)
                .expect("find high-performance adapter");
            let mut device = None;
            D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device)
                .expect("create D3D12 device");
            let device: ID3D12Device = device.expect("D3D12CreateDevice returned no device");
            let source = create_texture(
                &device,
                1280,
                720,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                D3D12_RESOURCE_FLAG_NONE,
            )
            .expect("create encoder source");
            let ready = device
                .CreateFence(1, D3D12_FENCE_FLAG_NONE)
                .expect("create ready fence");
            let completion = device
                .CreateFence(0, D3D12_FENCE_FLAG_NONE)
                .expect("create completion fence");
            let mut frame = D3d12Frame {
                device: device.clone(),
                resource: source,
                state: D3D12_RESOURCE_STATE_COMMON,
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                ready_fence: ready,
                ready_value: 1,
                completion_fence: completion,
                completion_value: 1,
                completion_submitted: false,
            };
            let mut encoder = Encoder::new_for_frame(&frame, 1280, 720, 2_000_000, 30.0)
                .expect("initialize D3D12 H.264 encoder");
            let encoded = encoder
                .encode_frame(&mut frame, 1280, 720)
                .expect("encode D3D12 H.264 frame");
            assert!(encoded.windows(3).any(|bytes| bytes == [0, 0, 1]));
            let mut decoder = openh264::decoder::Decoder::new().expect("initialize H.264 decoder");
            assert!(
                decoder
                    .decode(&encoded)
                    .expect("decode D3D12 H.264 output")
                    .is_some()
            );
        }
    }
}

macro_rules! transition_fn {
    ($name:ident, $list:ty) => {
        unsafe fn $name(
            list: &$list,
            resource: &ID3D12Resource,
            before: D3D12_RESOURCE_STATES,
            after: D3D12_RESOURCE_STATES,
        ) {
            if before == after {
                return;
            }
            let mut barrier = D3D12_RESOURCE_BARRIER {
                Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
                Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
                Anonymous: D3D12_RESOURCE_BARRIER_0 {
                    Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                        pResource: ManuallyDrop::new(Some(resource.clone())),
                        Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                        StateBefore: before,
                        StateAfter: after,
                    }),
                },
            };
            unsafe {
                list.ResourceBarrier(std::slice::from_ref(&barrier));
                let transition = &mut *barrier.Anonymous.Transition;
                ManuallyDrop::drop(&mut transition.pResource);
            }
        }
    };
}

transition_fn!(graphics_transition, ID3D12GraphicsCommandList);
transition_fn!(process_transition, ID3D12VideoProcessCommandList);
transition_fn!(encode_transition, ID3D12VideoEncodeCommandList2);
