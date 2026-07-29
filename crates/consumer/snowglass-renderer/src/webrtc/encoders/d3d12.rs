//! D3D12 Video Process + Video Encode HEVC/H.264 backend.
#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    mem::{ManuallyDrop, size_of},
    ptr,
};

use windows::{
    Win32::{
        Foundation::{E_POINTER, HANDLE, RECT, WAIT_OBJECT_0},
        Graphics::{Direct3D12::*, Dxgi::Common::*},
        Media::MediaFoundation::*,
        System::Threading::{CreateEventW, WaitForSingleObject},
    },
    core::Interface,
};

use super::{super::D3d12Frame, Codec};

type Error = Box<dyn std::error::Error + Send + Sync>;

const BITSTREAM_PADDING: u64 = 64 * 1024;

struct CodecSupport {
    metadata_size: u64,
    hevc: Option<HevcSupport>,
}

/// Driver-specific HEVC encoder limits. The codec configuration and the
/// hand-written parameter sets must both be derived from these values: fixed
/// coding-unit or transform-unit sizes are rejected by drivers whose hardware
/// uses different block sizes.
#[derive(Clone, Copy)]
struct HevcSupport {
    configuration: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC,
    subregion_block_size: u32,
}

// windows-rs 0.61 predates this binary-compatible extension from current
// DirectX-Headers. Keeping the definition local lets us use feature query 48
// without forcing the rest of the workspace onto a new windows-rs release.
#[repr(C)]
struct CodecResourceRequirements1 {
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
    codec: Codec,
    _device: ID3D12Device,
    _video_device: ID3D12VideoDevice3,
    processor: ID3D12VideoProcessor,
    process_queue: ID3D12CommandQueue,
    process_allocator: ID3D12CommandAllocator,
    process_list: ID3D12VideoProcessCommandList,
    crop_texture: Option<ID3D12Resource>,
    crop_queue: Option<ID3D12CommandQueue>,
    crop_allocator: Option<ID3D12CommandAllocator>,
    crop_list: Option<ID3D12GraphicsCommandList>,
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
        preferred: Codec,
    ) -> Result<Self, Error> {
        if preferred == Codec::H264 {
            return Self::new_with_codec(frame, width, height, bitrate, frame_rate, Codec::H264);
        }
        match Self::new_with_codec(frame, width, height, bitrate, frame_rate, Codec::Hevc) {
            Ok(encoder) => Ok(encoder),
            Err(hevc_error) => {
                tracing::warn!("snowglass: D3D12 HEVC unavailable; trying H.264: {hevc_error}");
                Self::new_with_codec(frame, width, height, bitrate, frame_rate, Codec::H264)
            }
        }
    }

    fn new_with_codec(
        frame: &D3d12Frame,
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: f32,
        codec: Codec,
    ) -> Result<Self, Error> {
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            return Err("D3D12 video encoding requires a non-zero, even-sized frame".into());
        }
        let frame_rate = frame_rate.round().max(1.0) as u32;

        unsafe {
            let device = frame.device.clone();
            let video_device: ID3D12VideoDevice3 = device.cast()?;
            let support =
                query_codec_support(&video_device, width, height, bitrate, frame_rate, codec)?;
            let parameter_sets = match support.hevc.as_ref() {
                None => super::openh264::parameter_sets(width, height, bitrate, frame_rate as f32)?,
                Some(hevc) => hevc_parameter_sets(width, height, hevc),
            };
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
            let source_desc = frame.resource.GetDesc();
            let source_width = u32::try_from(source_desc.Width)
                .map_err(|_| "D3D12 video source width exceeds u32")?;
            let needs_crop = source_width != width || source_desc.Height != height;
            let (crop_texture, crop_queue, crop_allocator, crop_list) = if needs_crop {
                let texture = create_texture(
                    &device,
                    width,
                    height,
                    frame.format,
                    D3D12_RESOURCE_FLAG_NONE,
                )?;
                let queue = create_queue(&device, D3D12_COMMAND_LIST_TYPE_DIRECT)?;
                let allocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
                let list: ID3D12GraphicsCommandList = device.CreateCommandList(
                    0,
                    D3D12_COMMAND_LIST_TYPE_DIRECT,
                    &allocator,
                    None,
                )?;
                list.Close()?;
                (Some(texture), Some(queue), Some(allocator), Some(list))
            } else {
                (None, None, None, None)
            };
            let processor = create_processor(
                &video_device,
                frame.format,
                width,
                height,
                width,
                height,
                frame_rate,
            )?;

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

            let resolution = D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
                Width: width,
                Height: height,
            };
            let (encoder, encoder_heap) =
                create_codec_objects(&video_device, codec, &resolution, support.hevc.as_ref())?;

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
                codec,
                _device: device,
                _video_device: video_device,
                processor,
                process_queue,
                process_allocator,
                process_list,
                crop_texture,
                crop_queue,
                crop_allocator,
                crop_list,
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

    pub(super) fn codec(&self) -> Codec {
        self.codec
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
            self.submit_crop(frame)
                .map_err(|error| format!("D3D12 GPU crop failed: {error}"))?;
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

    unsafe fn submit_crop(&mut self, frame: &mut D3d12Frame) -> Result<(), Error> {
        unsafe {
            let (Some(texture), Some(queue), Some(allocator), Some(list)) = (
                self.crop_texture.as_ref(),
                self.crop_queue.as_ref(),
                self.crop_allocator.as_ref(),
                self.crop_list.as_ref(),
            ) else {
                return Ok(());
            };
            allocator.Reset()?;
            list.Reset(allocator, None)?;
            queue.Wait(&self.fence, self.fence_value)?;
            queue.Wait(&frame.ready_fence, frame.ready_value)?;
            graphics_transition(
                list,
                &frame.resource,
                frame.state,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
            );
            graphics_transition(
                list,
                texture,
                D3D12_RESOURCE_STATE_COMMON,
                D3D12_RESOURCE_STATE_COPY_DEST,
            );
            let mut destination = D3D12_TEXTURE_COPY_LOCATION {
                pResource: ManuallyDrop::new(Some(texture.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    SubresourceIndex: 0,
                },
            };
            let mut source = D3D12_TEXTURE_COPY_LOCATION {
                pResource: ManuallyDrop::new(Some(frame.resource.clone())),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                    SubresourceIndex: 0,
                },
            };
            let source_box = D3D12_BOX {
                left: 0,
                top: 0,
                front: 0,
                right: self.width,
                bottom: self.height,
                back: 1,
            };
            list.CopyTextureRegion(&destination, 0, 0, 0, &source, Some(&raw const source_box));
            ManuallyDrop::drop(&mut destination.pResource);
            ManuallyDrop::drop(&mut source.pResource);
            graphics_transition(
                list,
                &frame.resource,
                D3D12_RESOURCE_STATE_COPY_SOURCE,
                frame.state,
            );
            graphics_transition(
                list,
                texture,
                D3D12_RESOURCE_STATE_COPY_DEST,
                D3D12_RESOURCE_STATE_COMMON,
            );
            list.Close()?;
            let command: ID3D12CommandList = list.cast()?;
            queue.ExecuteCommandLists(&[Some(command)]);
            self.fence_value += 1;
            queue.Signal(&self.fence, self.fence_value)?;
            queue.Signal(&frame.completion_fence, frame.completion_value)?;
            frame.mark_completion_submitted();
            Ok(())
        }
    }

    unsafe fn submit_conversion(&mut self, frame: &mut D3d12Frame) -> Result<(), Error> {
        unsafe {
            self.process_allocator.Reset()?;
            self.process_list.Reset(&self.process_allocator)?;
            let input_resource = self
                .crop_texture
                .as_ref()
                .cloned()
                .unwrap_or_else(|| frame.resource.clone());
            self.process_queue.Wait(&self.fence, self.fence_value)?;
            if self.crop_texture.is_none() {
                self.process_queue
                    .Wait(&frame.ready_fence, frame.ready_value)?;
            }
            process_transition(
                &self.process_list,
                &input_resource,
                if self.crop_texture.is_some() {
                    D3D12_RESOURCE_STATE_COMMON
                } else {
                    frame.state
                },
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
            input.InputStream[0].pTexture2D = ManuallyDrop::new(Some(input_resource.clone()));
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
                &input_resource,
                D3D12_RESOURCE_STATE_VIDEO_PROCESS_READ,
                if self.crop_texture.is_some() {
                    D3D12_RESOURCE_STATE_COMMON
                } else {
                    frame.state
                },
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
            if self.crop_texture.is_none() {
                self.process_queue
                    .Signal(&frame.completion_fence, frame.completion_value)?;
                frame.mark_completion_submitted();
            }
            Ok(())
        }
    }

    unsafe fn submit_encode(&mut self) -> Result<(), Error> {
        if self.codec == Codec::Hevc {
            return unsafe { self.submit_hevc_encode() };
        }
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

    unsafe fn submit_hevc_encode(&mut self) -> Result<(), Error> {
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
            let mut gop = D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_HEVC {
                GOPLength: 1,
                PPicturePeriod: 0,
                log2_max_pic_order_cnt_lsb_minus4: 4,
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
                    DataSize: size_of::<D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_HEVC>() as u32,
                    Anonymous: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_0 {
                        pHEVCGroupOfPictures: &raw mut gop,
                    },
                },
                ..Default::default()
            };
            let mut hevc_picture = D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_HEVC {
                Flags: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_HEVC_FLAG_NONE,
                FrameType: D3D12_VIDEO_ENCODER_FRAME_TYPE_HEVC_IDR_FRAME,
                ..Default::default()
            };
            let picture = D3D12_VIDEO_ENCODER_PICTURE_CONTROL_DESC {
                Flags: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_FLAG_USED_AS_REFERENCE_PICTURE,
                PictureControlCodecData: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA {
                    DataSize: size_of::<D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_HEVC>()
                        as u32,
                    Anonymous: D3D12_VIDEO_ENCODER_PICTURE_CONTROL_CODEC_DATA_0 {
                        pHEVCPicData: &raw mut hevc_picture,
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
                    FrameStartOffset: 0,
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
            for resource in [&self.reconstructed, &self.bitstream, &self.encoded_metadata] {
                encode_transition(
                    &self.encode_list,
                    resource,
                    D3D12_RESOURCE_STATE_VIDEO_ENCODE_WRITE,
                    D3D12_RESOURCE_STATE_COMMON,
                );
            }
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

unsafe fn create_codec_objects(
    device: &ID3D12VideoDevice3,
    codec: Codec,
    resolution: &D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC,
    hevc: Option<&HevcSupport>,
) -> Result<(ID3D12VideoEncoder, ID3D12VideoEncoderHeap), Error> {
    unsafe {
        match codec {
            Codec::H264 => {
                let mut profile = D3D12_VIDEO_ENCODER_PROFILE_H264_MAIN;
                let profile = profile_desc(&mut profile);
                let mut configuration = h264_configuration();
                let encoder_desc = D3D12_VIDEO_ENCODER_DESC {
                    NodeMask: 0,
                    Flags: D3D12_VIDEO_ENCODER_FLAG_NONE,
                    EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_H264,
                    EncodeProfile: profile,
                    InputFormat: DXGI_FORMAT_NV12,
                    CodecConfiguration: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION {
                        DataSize: size_of::<D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264>() as u32,
                        Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_0 {
                            pH264Config: &raw mut configuration,
                        },
                    },
                    MaxMotionEstimationPrecision:
                        D3D12_VIDEO_ENCODER_MOTION_ESTIMATION_PRECISION_MODE_MAXIMUM,
                };
                let encoder =
                    device.CreateVideoEncoder::<ID3D12VideoEncoder>(&raw const encoder_desc)?;
                let mut level = D3D12_VIDEO_ENCODER_LEVELS_H264_41;
                let heap_desc = D3D12_VIDEO_ENCODER_HEAP_DESC {
                    NodeMask: 0,
                    Flags: D3D12_VIDEO_ENCODER_HEAP_FLAG_NONE,
                    EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_H264,
                    EncodeProfile: profile,
                    EncodeLevel: level_desc(&mut level),
                    ResolutionsListCount: 1,
                    pResolutionList: resolution,
                };
                let heap = device
                    .CreateVideoEncoderHeap::<ID3D12VideoEncoderHeap>(&raw const heap_desc)?;
                Ok((encoder, heap))
            }
            Codec::Hevc => {
                let mut profile = D3D12_VIDEO_ENCODER_PROFILE_HEVC_MAIN;
                let profile = hevc_profile_desc(&mut profile);
                let mut configuration = hevc
                    .ok_or("D3D12 HEVC encoder creation requires queried configuration limits")?
                    .configuration;
                let encoder_desc = D3D12_VIDEO_ENCODER_DESC {
                    NodeMask: 0,
                    Flags: D3D12_VIDEO_ENCODER_FLAG_NONE,
                    EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
                    EncodeProfile: profile,
                    InputFormat: DXGI_FORMAT_NV12,
                    CodecConfiguration: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION {
                        DataSize: size_of::<D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC>() as u32,
                        Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_0 {
                            pHEVCConfig: &raw mut configuration,
                        },
                    },
                    MaxMotionEstimationPrecision:
                        D3D12_VIDEO_ENCODER_MOTION_ESTIMATION_PRECISION_MODE_MAXIMUM,
                };
                let encoder =
                    device.CreateVideoEncoder::<ID3D12VideoEncoder>(&raw const encoder_desc)?;
                let mut level = D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC {
                    Level: D3D12_VIDEO_ENCODER_LEVELS_HEVC_51,
                    Tier: D3D12_VIDEO_ENCODER_TIER_HEVC_MAIN,
                };
                let heap_desc = D3D12_VIDEO_ENCODER_HEAP_DESC {
                    NodeMask: 0,
                    Flags: D3D12_VIDEO_ENCODER_HEAP_FLAG_NONE,
                    EncodeCodec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
                    EncodeProfile: profile,
                    EncodeLevel: hevc_level_desc(&mut level),
                    ResolutionsListCount: 1,
                    pResolutionList: resolution,
                };
                let heap = device
                    .CreateVideoEncoderHeap::<ID3D12VideoEncoderHeap>(&raw const heap_desc)?;
                Ok((encoder, heap))
            }
        }
    }
}

unsafe fn query_hevc_support(
    device: &ID3D12VideoDevice3,
    width: u32,
    height: u32,
    bitrate: u32,
    frame_rate: u32,
) -> Result<CodecSupport, Error> {
    unsafe {
        let mut codec_support = D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC {
            NodeIndex: 0,
            Codec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
            ..Default::default()
        };
        check_video_feature(
            device,
            D3D12_FEATURE_VIDEO_ENCODER_CODEC,
            &mut codec_support,
        )?;
        if !codec_support.IsSupported.as_bool() {
            return Err("adapter does not support D3D12 HEVC video encoding".into());
        }
        let mut profile = D3D12_VIDEO_ENCODER_PROFILE_HEVC_MAIN;
        let mut min_level = D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC::default();
        let mut max_level = D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC::default();
        let mut profile_support = D3D12_FEATURE_DATA_VIDEO_ENCODER_PROFILE_LEVEL {
            NodeIndex: 0,
            Codec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
            Profile: hevc_profile_desc(&mut profile),
            MinSupportedLevel: hevc_level_desc(&mut min_level),
            MaxSupportedLevel: hevc_level_desc(&mut max_level),
            ..Default::default()
        };
        check_video_feature(
            device,
            D3D12_FEATURE_VIDEO_ENCODER_PROFILE_LEVEL,
            &mut profile_support,
        )?;
        if !profile_support.IsSupported.as_bool() {
            return Err("adapter does not support the HEVC Main profile".into());
        }
        let mut input_support = D3D12_FEATURE_DATA_VIDEO_ENCODER_INPUT_FORMAT {
            NodeIndex: 0,
            Codec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
            Profile: hevc_profile_desc(&mut profile),
            Format: DXGI_FORMAT_NV12,
            ..Default::default()
        };
        check_video_feature(
            device,
            D3D12_FEATURE_VIDEO_ENCODER_INPUT_FORMAT,
            &mut input_support,
        )?;
        if !input_support.IsSupported.as_bool() {
            return Err("adapter does not support NV12 HEVC Main video encoding".into());
        }
        let resolution = D3D12_VIDEO_ENCODER_PICTURE_RESOLUTION_DESC {
            Width: width,
            Height: height,
        };
        let mut requirements = D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOURCE_REQUIREMENTS {
            NodeIndex: 0,
            Codec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
            Profile: hevc_profile_desc(&mut profile),
            InputFormat: DXGI_FORMAT_NV12,
            PictureTargetResolution: resolution,
            ..Default::default()
        };
        check_video_feature(
            device,
            D3D12_FEATURE_VIDEO_ENCODER_RESOURCE_REQUIREMENTS,
            &mut requirements,
        )?;
        if !requirements.IsSupported.as_bool() {
            return Err(format!("adapter rejected D3D12 HEVC encoding at {width}x{height}").into());
        }

        // The coding-unit and transform-unit ranges are hardware-specific and
        // this query is a proposal: the caller fills in a candidate
        // configuration and the driver answers whether it can encode with it.
        // Try common configurations in preference order; the encoder and the
        // hand-written SPS both have to use the accepted one.
        let candidates: [(i32, i32, i32, i32, u8); 6] = [
            (0, 2, 0, 3, 3), // CU 8..32, TU 4..32 (NVIDIA)
            (1, 2, 0, 3, 3), // CU 16..32
            (0, 3, 0, 3, 3), // CU 8..64
            (1, 3, 0, 3, 3), // CU 16..64
            (0, 3, 0, 2, 2), // CU 8..64, TU 4..16
            (2, 2, 0, 3, 3), // CU 32..32
        ];
        let mut accepted = None;
        for (min_cu, max_cu, min_tu, max_tu, depth) in candidates {
            let mut configuration_limits = D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_HEVC {
                SupportFlags: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_HEVC_FLAGS(0),
                MinLumaCodingUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_CUSIZE(min_cu),
                MaxLumaCodingUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_CUSIZE(max_cu),
                MinLumaTransformUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_TUSIZE(
                    min_tu,
                ),
                MaxLumaTransformUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_TUSIZE(
                    max_tu,
                ),
                max_transform_hierarchy_depth_inter: depth,
                max_transform_hierarchy_depth_intra: depth,
            };
            let mut configuration_support =
                D3D12_FEATURE_DATA_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT {
                    NodeIndex: 0,
                    Codec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
                    Profile: hevc_profile_desc(&mut profile),
                    CodecSupportLimits: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT {
                        DataSize: size_of::<D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_HEVC>()
                            as u32,
                        Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_0 {
                            pHEVCSupport: &raw mut configuration_limits,
                        },
                    },
                    ..Default::default()
                };
            check_video_feature(
                device,
                D3D12_FEATURE_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT,
                &mut configuration_support,
            )?;
            if configuration_support.IsSupported.as_bool() {
                accepted = Some(configuration_limits);
                break;
            }
        }
        let Some(configuration_limits) = accepted else {
            return Err("adapter accepted no candidate D3D12 HEVC configuration".into());
        };
        let mut configuration_flags = D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_FLAG_NONE;
        if configuration_limits.SupportFlags.0
            & D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_SUPPORT_HEVC_FLAG_ASYMETRIC_MOTION_PARTITION_REQUIRED.0
            != 0
        {
            configuration_flags = D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_FLAG_USE_ASYMETRIC_MOTION_PARTITION;
        }
        let mut configuration = D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC {
            ConfigurationFlags: configuration_flags,
            MinLumaCodingUnitSize: configuration_limits.MinLumaCodingUnitSize,
            MaxLumaCodingUnitSize: configuration_limits.MaxLumaCodingUnitSize,
            MinLumaTransformUnitSize: configuration_limits.MinLumaTransformUnitSize,
            MaxLumaTransformUnitSize: configuration_limits.MaxLumaTransformUnitSize,
            max_transform_hierarchy_depth_inter: configuration_limits
                .max_transform_hierarchy_depth_inter,
            max_transform_hierarchy_depth_intra: configuration_limits
                .max_transform_hierarchy_depth_intra,
        };

        // The combined query validates the exact codec, GOP, rate control and
        // resolution combination and reports the subregion block size the
        // parameter sets must align the picture dimensions to.
        let mut gop = D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_HEVC {
            GOPLength: 1,
            PPicturePeriod: 0,
            log2_max_pic_order_cnt_lsb_minus4: 4,
        };
        let cbr = D3D12_VIDEO_ENCODER_RATE_CONTROL_CBR {
            TargetBitRate: bitrate as u64,
            ..Default::default()
        };
        let mut resolution_limits =
            D3D12_FEATURE_DATA_VIDEO_ENCODER_RESOLUTION_SUPPORT_LIMITS::default();
        let mut suggested_profile = D3D12_VIDEO_ENCODER_PROFILE_HEVC_MAIN;
        let mut suggested_level = D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC {
            Level: D3D12_VIDEO_ENCODER_LEVELS_HEVC_51,
            Tier: D3D12_VIDEO_ENCODER_TIER_HEVC_MAIN,
        };
        let mut session_support = D3D12_FEATURE_DATA_VIDEO_ENCODER_SUPPORT1 {
            NodeIndex: 0,
            Codec: D3D12_VIDEO_ENCODER_CODEC_HEVC,
            InputFormat: DXGI_FORMAT_NV12,
            CodecConfiguration: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION {
                DataSize: size_of::<D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC>() as u32,
                Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_0 {
                    pHEVCConfig: &raw mut configuration,
                },
            },
            CodecGopSequence: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE {
                DataSize: size_of::<D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_HEVC>() as u32,
                Anonymous: D3D12_VIDEO_ENCODER_SEQUENCE_GOP_STRUCTURE_0 {
                    pHEVCGroupOfPictures: &raw mut gop,
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
            SuggestedProfile: hevc_profile_desc(&mut suggested_profile),
            SuggestedLevel: hevc_level_desc(&mut suggested_level),
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
                "adapter rejected D3D12 HEVC session (validation flags {:#x})",
                session_support.ValidationFlags.0
            )
            .into());
        }

        let mut requirements1: CodecResourceRequirements1 = std::mem::zeroed();
        requirements1.codec = D3D12_VIDEO_ENCODER_CODEC_HEVC;
        requirements1.profile = hevc_profile_desc(&mut profile);
        requirements1.input_format = DXGI_FORMAT_NV12;
        requirements1.picture_target_resolution = resolution;
        requirements1.codec_configuration = D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION {
            DataSize: size_of::<D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC>() as u32,
            Anonymous: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_0 {
                pHEVCConfig: &raw mut configuration,
            },
        };
        check_video_feature(device, D3D12_FEATURE_VIDEO(48), &mut requirements1)?;
        if !requirements1.is_supported.as_bool() {
            return Err("adapter rejected the extended D3D12 HEVC resource requirements".into());
        }
        Ok(CodecSupport {
            metadata_size: align_up(
                requirements1.max_encoder_output_metadata_buffer_size as u64,
                requirements1
                    .encoder_metadata_buffer_access_alignment
                    .max(1) as u64,
            ),
            hevc: Some(HevcSupport {
                configuration,
                subregion_block_size: resolution_limits.SubregionBlockPixelsSize.max(8),
            }),
        })
    }
}

unsafe fn query_codec_support(
    device: &ID3D12VideoDevice3,
    width: u32,
    height: u32,
    bitrate: u32,
    frame_rate: u32,
    codec: Codec,
) -> Result<CodecSupport, Error> {
    unsafe {
        match codec {
            Codec::Hevc => query_hevc_support(device, width, height, bitrate, frame_rate),
            Codec::H264 => query_h264_support(device, width, height, bitrate, frame_rate),
        }
    }
}

unsafe fn query_h264_support(
    device: &ID3D12VideoDevice3,
    width: u32,
    height: u32,
    bitrate: u32,
    frame_rate: u32,
) -> Result<CodecSupport, Error> {
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
        let mut requirements1: CodecResourceRequirements1 = std::mem::zeroed();
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

        Ok(CodecSupport {
            metadata_size: align_up(
                requirements1.max_encoder_output_metadata_buffer_size as u64,
                requirements1
                    .encoder_metadata_buffer_access_alignment
                    .max(1) as u64,
            ),
            hevc: None,
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
    input_width: u32,
    input_height: u32,
    output_width: u32,
    output_height: u32,
    frame_rate: u32,
) -> Result<ID3D12VideoProcessor, Error> {
    let source_ratio = DXGI_RATIONAL {
        Numerator: input_width,
        Denominator: input_height,
    };
    let destination_ratio = DXGI_RATIONAL {
        Numerator: output_width,
        Denominator: output_height,
    };
    let rate = DXGI_RATIONAL {
        Numerator: frame_rate,
        Denominator: 1,
    };
    let source_size = D3D12_VIDEO_SIZE_RANGE {
        MaxWidth: input_width,
        MaxHeight: input_height,
        MinWidth: input_width,
        MinHeight: input_height,
    };
    let destination_size = D3D12_VIDEO_SIZE_RANGE {
        MaxWidth: output_width,
        MaxHeight: output_height,
        MinWidth: output_width,
        MinHeight: output_height,
    };
    let input = D3D12_VIDEO_PROCESS_INPUT_STREAM_DESC {
        Format: input_format,
        ColorSpace: DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
        SourceAspectRatio: source_ratio,
        DestinationAspectRatio: destination_ratio,
        FrameRate: rate,
        SourceSizeRange: source_size,
        DestinationSizeRange: destination_size,
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

fn hevc_profile_desc(
    profile: &mut D3D12_VIDEO_ENCODER_PROFILE_HEVC,
) -> D3D12_VIDEO_ENCODER_PROFILE_DESC {
    D3D12_VIDEO_ENCODER_PROFILE_DESC {
        DataSize: size_of::<D3D12_VIDEO_ENCODER_PROFILE_HEVC>() as u32,
        Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 {
            pHEVCProfile: profile,
        },
    }
}

fn hevc_level_desc(
    level: &mut D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC,
) -> D3D12_VIDEO_ENCODER_LEVEL_SETTING {
    D3D12_VIDEO_ENCODER_LEVEL_SETTING {
        DataSize: size_of::<D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC>() as u32,
        Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 {
            pHEVCLevelSetting: level,
        },
    }
}

fn h264_configuration() -> D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264 {
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264 {
        ConfigurationFlags: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_FLAG_NONE,
        DirectModeConfig: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_DIRECT_MODES_DISABLED,
        DisableDeblockingFilterConfig:
            D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_SLICES_DEBLOCKING_MODE_0_ALL_LUMA_CHROMA_SLICE_BLOCK_EDGES_ALWAYS_FILTERED,
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

/// Bounded wait: an encode pipeline that wedged (or a device that was
/// removed) must surface as an error instead of freezing the encoder thread.
const GPU_WAIT_TIMEOUT_MS: u32 = 5_000;

unsafe fn wait_fence(fence: &ID3D12Fence, value: u64, event: HANDLE) -> Result<(), Error> {
    unsafe {
        if value != 0 && fence.GetCompletedValue() < value {
            fence.SetEventOnCompletion(value, event)?;
            if WaitForSingleObject(event, GPU_WAIT_TIMEOUT_MS) != WAIT_OBJECT_0 {
                return Err("timed out waiting for D3D12 video fence".into());
            }
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

/// Builds the VPS/SPS/PPS that D3D12 intentionally leaves to the host. The
/// values mirror the all-intra Main-profile configuration submitted below.
/// Writes Annex-B VPS/SPS/PPS matching the D3D12 encoder configuration. The
/// encoder emits only slice data, so every SPS field that the hardware bakes
/// into slices (coding-unit range, transform range, AMP) must agree with
/// `support.configuration`, and the picture dimensions must be aligned to the
/// subregion block size with a conformance window covering the padding.
fn hevc_parameter_sets(width: u32, height: u32, support: &HevcSupport) -> Vec<u8> {
    let configuration = &support.configuration;
    let min_cu_log2 = configuration.MinLumaCodingUnitSize.0 as u32 + 3;
    let max_cu_log2 = configuration.MaxLumaCodingUnitSize.0 as u32 + 3;
    let min_tu_log2 = configuration.MinLumaTransformUnitSize.0 as u32 + 2;
    let max_tu_log2 = configuration.MaxLumaTransformUnitSize.0 as u32 + 2;
    let amp_enabled = configuration.ConfigurationFlags.0
        & D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_FLAG_USE_ASYMETRIC_MOTION_PARTITION.0
        != 0;
    let alignment = support.subregion_block_size.max(1 << min_cu_log2) as u64;
    let coded_width = align_up(width as u64, alignment) as u32;
    let coded_height = align_up(height as u64, alignment) as u32;
    let mut output = Vec::new();

    let mut vps = BitWriter::default();
    vps.bits(0, 4);
    vps.bit(true);
    vps.bit(true);
    vps.bits(0, 6);
    vps.bits(0, 3);
    vps.bit(true);
    vps.bits(0xffff, 16);
    hevc_profile_tier_level(&mut vps);
    vps.bit(false);
    vps.ue(0);
    vps.ue(0);
    vps.ue(0);
    vps.bits(0, 6);
    vps.ue(0);
    vps.bit(false);
    vps.bit(false);
    append_hevc_nal(&mut output, 32, vps.finish());

    let mut sps = BitWriter::default();
    sps.bits(0, 4);
    sps.bits(0, 3);
    sps.bit(true);
    hevc_profile_tier_level(&mut sps);
    sps.ue(0); // sps_seq_parameter_set_id
    sps.ue(1); // chroma_format_idc: 4:2:0
    sps.ue(coded_width); // pic_width_in_luma_samples
    sps.ue(coded_height); // pic_height_in_luma_samples
    let cropped = coded_width != width || coded_height != height;
    sps.bit(cropped); // conformance_window_flag
    if cropped {
        sps.ue(0); // conf_win_left_offset
        sps.ue((coded_width - width) / 2); // conf_win_right_offset (chroma units)
        sps.ue(0); // conf_win_top_offset
        sps.ue((coded_height - height) / 2); // conf_win_bottom_offset
    }
    sps.ue(0); // bit_depth_luma_minus8
    sps.ue(0); // bit_depth_chroma_minus8
    sps.ue(4); // log2_max_pic_order_cnt_lsb_minus4
    sps.bit(false); // sps_sub_layer_ordering_info_present_flag
    sps.ue(0); // sps_max_dec_pic_buffering_minus1
    sps.ue(0); // sps_max_num_reorder_pics
    sps.ue(0); // sps_max_latency_increase_plus1
    sps.ue(min_cu_log2 - 3); // log2_min_luma_coding_block_size_minus3
    sps.ue(max_cu_log2 - min_cu_log2); // log2_diff_max_min_luma_coding_block_size
    sps.ue(min_tu_log2 - 2); // log2_min_luma_transform_block_size_minus2
    sps.ue(max_tu_log2 - min_tu_log2); // log2_diff_max_min_luma_transform_block_size
    sps.ue(configuration.max_transform_hierarchy_depth_inter as u32);
    sps.ue(configuration.max_transform_hierarchy_depth_intra as u32);
    sps.bit(false); // scaling_list_enabled_flag
    sps.bit(amp_enabled); // amp_enabled_flag
    sps.bit(false); // sample_adaptive_offset_enabled_flag
    sps.bit(false); // pcm_enabled_flag
    sps.ue(0); // num_short_term_ref_pic_sets
    sps.bit(false); // long_term_ref_pics_present_flag
    sps.bit(false); // sps_temporal_mvp_enabled_flag
    sps.bit(true); // strong_intra_smoothing_enabled_flag
    sps.bit(false); // vui_parameters_present_flag
    sps.bit(false); // sps_extension_present_flag
    append_hevc_nal(&mut output, 33, sps.finish());

    let mut pps = BitWriter::default();
    pps.ue(0);
    pps.ue(0);
    pps.bit(false);
    pps.bit(false);
    pps.bits(0, 3);
    pps.bit(false);
    pps.bit(false);
    pps.ue(0);
    pps.ue(0);
    pps.se(0);
    pps.bit(false);
    pps.bit(false);
    pps.bit(false);
    pps.se(0);
    pps.se(0);
    pps.bit(false);
    pps.bit(false);
    pps.bit(false);
    pps.bit(false);
    pps.bit(false);
    pps.bit(false);
    pps.bit(true);
    pps.bit(true);
    pps.bit(false);
    pps.bit(false);
    pps.se(0);
    pps.se(0);
    pps.bit(false);
    pps.bit(false);
    pps.ue(0);
    pps.bit(false);
    pps.bit(false);
    append_hevc_nal(&mut output, 34, pps.finish());
    output
}

fn hevc_profile_tier_level(bits: &mut BitWriter) {
    bits.bits(0, 2);
    bits.bit(false);
    bits.bits(1, 5);
    bits.bits(0x4000_0000, 32);
    bits.bit(true);
    bits.bit(false);
    bits.bit(false);
    bits.bit(true);
    bits.bits(0, 32);
    bits.bits(0, 12);
    bits.bits(153, 8);
}

fn append_hevc_nal(output: &mut Vec<u8>, nal_type: u8, rbsp: Vec<u8>) {
    output.extend_from_slice(&[0, 0, 0, 1, nal_type << 1, 1]);
    let mut zero_count = 0;
    for byte in rbsp {
        if zero_count >= 2 && byte <= 3 {
            output.push(3);
            zero_count = 0;
        }
        output.push(byte);
        zero_count = if byte == 0 { zero_count + 1 } else { 0 };
    }
}

#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    current: u8,
    used: u8,
}

impl BitWriter {
    fn bit(&mut self, value: bool) {
        self.current |= u8::from(value) << (7 - self.used);
        self.used += 1;
        if self.used == 8 {
            self.bytes.push(self.current);
            self.current = 0;
            self.used = 0;
        }
    }

    fn bits(&mut self, value: u64, count: u8) {
        for shift in (0..count).rev() {
            self.bit((value >> shift) & 1 != 0);
        }
    }

    fn ue(&mut self, value: u32) {
        let code = value as u64 + 1;
        let width = 64 - code.leading_zeros() as u8;
        self.bits(0, width - 1);
        self.bits(code, width);
    }

    fn se(&mut self, value: i32) {
        let code = if value <= 0 {
            value.unsigned_abs() * 2
        } else {
            value as u32 * 2 - 1
        };
        self.ue(code);
    }

    fn finish(mut self) -> Vec<u8> {
        self.bit(true);
        while self.used != 0 {
            self.bit(false);
        }
        self.bytes
    }
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

    fn test_hevc_support() -> HevcSupport {
        HevcSupport {
            configuration: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC {
                ConfigurationFlags: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_FLAG_NONE,
                MinLumaCodingUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_CUSIZE_8x8,
                MaxLumaCodingUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_CUSIZE_32x32,
                MinLumaTransformUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_TUSIZE_4x4,
                MaxLumaTransformUnitSize: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_TUSIZE_32x32,
                max_transform_hierarchy_depth_inter: 3,
                max_transform_hierarchy_depth_intra: 3,
            },
            subregion_block_size: 32,
        }
    }

    #[test]
    fn hevc_parameter_sets_are_annex_b_vps_sps_pps() {
        let bytes = hevc_parameter_sets(1920, 1080, &test_hevc_support());
        let types = bytes
            .windows(5)
            .filter_map(|window| (window[..4] == [0, 0, 0, 1]).then_some(window[4] >> 1))
            .collect::<Vec<_>>();
        assert_eq!(types, [32, 33, 34]);
    }

    #[test]
    #[ignore = "requires a D3D12 adapter with HEVC video encoding"]
    fn hevc_init_probe() {
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
            for (width, height) in [(626u32, 632u32), (640, 480), (1280, 720)] {
                let source = create_texture(
                    &device,
                    width + 1,
                    height + 1,
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
                match Encoder::new_with_codec(&frame, width, height, 2_000_000, 30.0, Codec::Hevc) {
                    Ok(mut encoder) => {
                        tracing::info!("probe {width}x{height}: HEVC init OK");
                        let mut stream = Vec::new();
                        for index in 0..3 {
                            match encoder.encode_frame(&mut frame, width, height) {
                                Ok(encoded) => {
                                    tracing::info!("  frame {index}: {} bytes", encoded.len());
                                    stream.extend_from_slice(&encoded);
                                }
                                Err(error) => tracing::error!("  frame {index} failed: {error}"),
                            }
                        }
                        if let Some(path) = std::env::var_os("SNOWGLASS_HEVC_PROBE_DUMP") {
                            let path = std::path::PathBuf::from(path)
                                .join(format!("d3d12-{width}x{height}.h265"));
                            let _ = std::fs::write(path, &stream);
                        }
                    }
                    Err(error) => {
                        tracing::error!("probe {width}x{height}: HEVC init failed: {error}")
                    }
                }
                if let Ok(queue) =
                    device.cast::<windows::Win32::Graphics::Direct3D12::ID3D12InfoQueue>()
                {
                    let count = queue.GetNumStoredMessages();
                    for index in 0..count {
                        let mut size = 0;
                        if queue.GetMessage(index, None, &mut size).is_err() {
                            continue;
                        }
                        let mut buffer = vec![0u8; size];
                        let message = buffer
                            .as_mut_ptr()
                            .cast::<windows::Win32::Graphics::Direct3D12::D3D12_MESSAGE>(
                        );
                        if queue.GetMessage(index, Some(message), &mut size).is_ok() {
                            let message = &*message;
                            let text = std::slice::from_raw_parts(
                                message.pDescription.cast::<u8>(),
                                message.DescriptionByteLength.saturating_sub(1),
                            );
                            tracing::debug!("  d3d12: {}", String::from_utf8_lossy(text));
                        }
                    }
                    queue.ClearStoredMessages();
                }
            }
        }
    }

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
                627,
                633,
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
            let mut encoder =
                Encoder::new_with_codec(&frame, 626, 632, 2_000_000, 30.0, Codec::H264)
                    .expect("initialize D3D12 H.264 encoder");
            let mut decoder = openh264::decoder::Decoder::new().expect("initialize H.264 decoder");
            for frame_index in 0..60 {
                let encoded = encoder
                    .encode_frame(&mut frame, 626, 632)
                    .expect("encode D3D12 H.264 frame");
                assert!(encoded.windows(3).any(|bytes| bytes == [0, 0, 1]));
                assert!(
                    decoder
                        .decode(&encoded)
                        .unwrap_or_else(|error| panic!("decode frame {frame_index}: {error}"))
                        .is_some(),
                    "decoder returned no image for frame {frame_index}"
                );
            }
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
