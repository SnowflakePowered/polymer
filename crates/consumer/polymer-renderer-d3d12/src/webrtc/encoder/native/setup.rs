use super::*;

impl Encoder {
    pub(super) fn new_with_codec(
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
                None => BitWriter::h264_parameter_sets(width, height),
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
                fence_waiter: FenceWaiter::new()?,
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
}

pub(super) unsafe fn create_codec_objects(
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

pub(super) unsafe fn create_processor(
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

pub(super) unsafe fn create_queue(
    device: &ID3D12Device,
    kind: D3D12_COMMAND_LIST_TYPE,
) -> windows::core::Result<ID3D12CommandQueue> {
    unsafe {
        let queue: ID3D12CommandQueue = device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
            Type: kind,
            Priority: 0,
            Flags: D3D12_COMMAND_QUEUE_FLAG_NONE,
            NodeMask: 0,
        })?;
        queue.SetName(windows::core::w!("Polymer WebRTC queue"))?;
        Ok(queue)
    }
}

pub(super) unsafe fn create_texture(
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

pub(super) unsafe fn create_buffer(
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
