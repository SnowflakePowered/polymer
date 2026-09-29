use super::*;

impl VideoState {
    pub(super) unsafe fn new(
        frame: &VulkanFrame,
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: f32,
        codec: Codec,
    ) -> Result<Self, Error> {
        if codec == Codec::Hevc {
            return unsafe { Self::new_hevc(frame, width, height, bitrate, frame_rate) };
        }
        unsafe {
            let video_instance = ash::khr::video_queue::Instance::new(
                // The loader only retains function pointers and the instance handle.
                &ash::Entry::load()?,
                &frame.instance,
            );
            let video = ash::khr::video_queue::Device::new(&frame.instance, &frame.device);
            let encode = ash::khr::video_encode_queue::Device::new(&frame.instance, &frame.device);
            let mut h264_profile = vk::VideoEncodeH264ProfileInfoKHR::default().std_profile_idc(
                native::StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE,
            );
            let profile = vk::VideoProfileInfoKHR::default()
                .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H264)
                .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
                .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .push_next(&mut h264_profile);

            let mut h264_capabilities = vk::VideoEncodeH264CapabilitiesKHR::default();
            let mut encode_capabilities = vk::VideoEncodeCapabilitiesKHR::default();
            let mut capabilities = vk::VideoCapabilitiesKHR::default()
                .push_next(&mut h264_capabilities)
                .push_next(&mut encode_capabilities);
            vk_result((video_instance
                .fp()
                .get_physical_device_video_capabilities_khr)(
                frame.physical_device,
                &profile,
                &mut capabilities,
            ))
            .map_err(|error| format!("query H.264 capabilities: {error}"))?;
            let extent = vk::Extent2D { width, height };
            if width < capabilities.min_coded_extent.width
                || height < capabilities.min_coded_extent.height
                || width > capabilities.max_coded_extent.width
                || height > capabilities.max_coded_extent.height
            {
                return Err("frame extent is outside Vulkan Video H.264 capabilities".into());
            }
            let std_header_version = capabilities.std_header_version;
            let bitstream_alignment = capabilities.min_bitstream_buffer_size_alignment.max(1);
            let bitstream_offset_alignment =
                capabilities.min_bitstream_buffer_offset_alignment.max(1);
            if !encode_capabilities
                .supported_encode_feedback_flags
                .contains(vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN)
            {
                return Err("driver cannot report Vulkan Video bitstream byte counts".into());
            }
            let rate_control_mode = if encode_capabilities
                .rate_control_modes
                .contains(vk::VideoEncodeRateControlModeFlagsKHR::CBR)
            {
                vk::VideoEncodeRateControlModeFlagsKHR::CBR
            } else if encode_capabilities
                .rate_control_modes
                .contains(vk::VideoEncodeRateControlModeFlagsKHR::VBR)
            {
                vk::VideoEncodeRateControlModeFlagsKHR::VBR
            } else {
                vk::VideoEncodeRateControlModeFlagsKHR::DISABLED
            };

            let profiles = [profile];
            let mut profile_list = vk::VideoProfileListInfoKHR::default().profiles(&profiles);
            let format_info = vk::PhysicalDeviceVideoFormatInfoKHR::default()
                .image_usage(
                    vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR | vk::ImageUsageFlags::TRANSFER_DST,
                )
                .push_next(&mut profile_list);
            let mut format_count = 0;
            vk_result((video_instance
                .fp()
                .get_physical_device_video_format_properties_khr)(
                frame.physical_device,
                &format_info,
                &mut format_count,
                std::ptr::null_mut(),
            ))
            .map_err(|error| format!("count H.264 input formats: {error}"))?;
            if format_count == 0 {
                return Err("driver exposes no H.264 encode input format".into());
            }
            let mut formats = vec![vk::VideoFormatPropertiesKHR::default(); format_count as usize];
            vk_result((video_instance
                .fp()
                .get_physical_device_video_format_properties_khr)(
                frame.physical_device,
                &format_info,
                &mut format_count,
                formats.as_mut_ptr(),
            ))
            .map_err(|error| format!("query H.264 input formats: {error}"))?;
            let input_format = formats
                .iter()
                .find(|properties| properties.format == vk::Format::G8_B8R8_2PLANE_420_UNORM)
                .ok_or("driver does not expose two-plane NV12 H.264 input")?
                .format;

            let mut h264_session = vk::VideoEncodeH264SessionCreateInfoKHR::default()
                .use_max_level_idc(true)
                .max_level_idc(native::StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_1);
            let session_info = vk::VideoSessionCreateInfoKHR::default()
                .queue_family_index(frame.queue_family)
                .video_profile(&profile)
                .picture_format(input_format)
                .max_coded_extent(extent)
                .reference_picture_format(input_format)
                .max_dpb_slots(0)
                .max_active_reference_pictures(0)
                .std_header_version(&std_header_version)
                .push_next(&mut h264_session);
            let mut session = vk::VideoSessionKHR::null();
            vk_result((video.fp().create_video_session_khr)(
                frame.device.handle(),
                &session_info,
                std::ptr::null(),
                &mut session,
            ))
            .map_err(|error| format!("create H.264 video session: {error}"))?;

            let memory_properties = frame
                .instance
                .get_physical_device_memory_properties(frame.physical_device);
            let mut requirement_count = 0;
            vk_result((video.fp().get_video_session_memory_requirements_khr)(
                frame.device.handle(),
                session,
                &mut requirement_count,
                std::ptr::null_mut(),
            ))
            .map_err(|error| format!("count video session memory requirements: {error}"))?;
            let mut requirements =
                vec![vk::VideoSessionMemoryRequirementsKHR::default(); requirement_count as usize];
            vk_result((video.fp().get_video_session_memory_requirements_khr)(
                frame.device.handle(),
                session,
                &mut requirement_count,
                requirements.as_mut_ptr(),
            ))
            .map_err(|error| format!("query video session memory requirements: {error}"))?;
            let mut session_memory = Vec::with_capacity(requirements.len());
            let mut bindings = Vec::with_capacity(requirements.len());
            for requirement in &requirements {
                let memory_type = find_memory_type(
                    &memory_properties,
                    requirement.memory_requirements.memory_type_bits,
                    vk::MemoryPropertyFlags::DEVICE_LOCAL,
                )
                .or_else(|_| {
                    find_memory_type(
                        &memory_properties,
                        requirement.memory_requirements.memory_type_bits,
                        vk::MemoryPropertyFlags::empty(),
                    )
                })
                .map_err(|error| format!("find video session memory type: {error}"))?;
                let memory = frame.device.allocate_memory(
                    &vk::MemoryAllocateInfo::default()
                        .allocation_size(requirement.memory_requirements.size)
                        .memory_type_index(memory_type),
                    None,
                )?;
                bindings.push(
                    vk::BindVideoSessionMemoryInfoKHR::default()
                        .memory_bind_index(requirement.memory_bind_index)
                        .memory(memory)
                        .memory_offset(0)
                        .memory_size(requirement.memory_requirements.size),
                );
                session_memory.push(memory);
            }
            vk_result((video.fp().bind_video_session_memory_khr)(
                frame.device.handle(),
                session,
                bindings.len() as u32,
                bindings.as_ptr(),
            ))
            .map_err(|error| format!("bind video session memory: {error}"))?;

            let (sps, pps) = h264_parameter_structures(width, height);
            let sps_list = [sps];
            let pps_list = [pps];
            let add = vk::VideoEncodeH264SessionParametersAddInfoKHR::default()
                .std_sp_ss(&sps_list)
                .std_pp_ss(&pps_list);
            let mut h264_parameters = vk::VideoEncodeH264SessionParametersCreateInfoKHR::default()
                .max_std_sps_count(1)
                .max_std_pps_count(1)
                .parameters_add_info(&add);
            let parameters_info = vk::VideoSessionParametersCreateInfoKHR::default()
                .video_session(session)
                .push_next(&mut h264_parameters);
            let mut parameters = vk::VideoSessionParametersKHR::null();
            vk_result((video.fp().create_video_session_parameters_khr)(
                frame.device.handle(),
                &parameters_info,
                std::ptr::null(),
                &mut parameters,
            ))
            .map_err(|error| format!("create H.264 session parameters: {error}"))?;
            let parameter_sets = encoded_parameter_sets(&encode, &frame.device, parameters)
                .map_err(|error| format!("encode H.264 parameter sets: {error}"))?;
            let input = create_video_image(
                frame,
                width,
                height,
                input_format,
                vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR | vk::ImageUsageFlags::TRANSFER_DST,
                &profiles,
            )
            .map_err(|error| format!("create H.264 input image: {error}"))?;
            let bitstream_offset =
                (parameter_sets.len() as u64).next_multiple_of(bitstream_offset_alignment);
            let bitstream_size = (bitstream_offset + width as u64 * height as u64 * 3 + 64 * 1024)
                .next_multiple_of(bitstream_alignment);
            let bitstream = create_video_buffer(frame, bitstream_size, &profiles)
                .map_err(|error| format!("create H.264 bitstream buffer: {error}"))?;
            let mut feedback = vk::QueryPoolVideoEncodeFeedbackCreateInfoKHR::default()
                .encode_feedback_flags(vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN);
            let mut query_profile = profile;
            let query_info = vk::QueryPoolCreateInfo::default()
                .query_type(vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR)
                .query_count(1)
                .push_next(&mut query_profile)
                .push_next(&mut feedback);
            let query_pool = frame
                .device
                .create_query_pool(&query_info, None)
                .map_err(|error| format!("create video feedback query: {error}"))?;
            let command_pool = frame.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                    .queue_family_index(frame.queue_family),
                None,
            )?;
            let command = frame.device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0];
            let fence = frame.device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?;

            Ok(Self {
                codec: Codec::H264,
                dpb: None,
                device: frame.device.clone(),
                video,
                encode,
                session,
                parameters,
                session_memory,
                input,
                parameter_sets,
                queue: frame.queue,
                command_pool,
                command,
                fence,
                fence_in_flight: false,
                bitstream,
                bitstream_offset,
                query_pool,
                width,
                height,
                bitrate: (bitrate as u64).min(encode_capabilities.max_bitrate),
                frame_rate: frame_rate.round().max(1.0) as u32,
                rate_control_mode,
                initialized: false,
            })
        }
    }

    pub(super) unsafe fn new_hevc(
        frame: &VulkanFrame,
        width: u32,
        height: u32,
        bitrate: u32,
        frame_rate: f32,
    ) -> Result<Self, Error> {
        unsafe {
            let video_instance =
                ash::khr::video_queue::Instance::new(&ash::Entry::load()?, &frame.instance);
            let video = ash::khr::video_queue::Device::new(&frame.instance, &frame.device);
            let encode = ash::khr::video_encode_queue::Device::new(&frame.instance, &frame.device);
            let mut hevc_profile = vk::VideoEncodeH265ProfileInfoKHR::default()
                .std_profile_idc(native::StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN);
            let profile = vk::VideoProfileInfoKHR::default()
                .video_codec_operation(vk::VideoCodecOperationFlagsKHR::ENCODE_H265)
                .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
                .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
                .push_next(&mut hevc_profile);

            let mut hevc_capabilities = vk::VideoEncodeH265CapabilitiesKHR::default();
            let mut encode_capabilities = vk::VideoEncodeCapabilitiesKHR::default();
            let mut capabilities = vk::VideoCapabilitiesKHR::default()
                .push_next(&mut hevc_capabilities)
                .push_next(&mut encode_capabilities);
            vk_result((video_instance
                .fp()
                .get_physical_device_video_capabilities_khr)(
                frame.physical_device,
                &profile,
                &mut capabilities,
            ))
            .map_err(|error| format!("query HEVC capabilities: {error}"))?;
            let extent = vk::Extent2D { width, height };
            if width < capabilities.min_coded_extent.width
                || height < capabilities.min_coded_extent.height
                || width > capabilities.max_coded_extent.width
                || height > capabilities.max_coded_extent.height
            {
                return Err("frame extent is outside Vulkan Video HEVC capabilities".into());
            }
            let bitstream_alignment = capabilities.min_bitstream_buffer_size_alignment.max(1);
            let bitstream_offset_alignment =
                capabilities.min_bitstream_buffer_offset_alignment.max(1);
            let std_header_version = capabilities.std_header_version;
            if !encode_capabilities
                .supported_encode_feedback_flags
                .contains(vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN)
            {
                return Err("driver cannot report Vulkan Video HEVC byte counts".into());
            }
            let rate_control_mode = preferred_rate_control(&encode_capabilities);

            let profiles = [profile];
            let mut profile_list = vk::VideoProfileListInfoKHR::default().profiles(&profiles);
            let format_info = vk::PhysicalDeviceVideoFormatInfoKHR::default()
                .image_usage(
                    vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR | vk::ImageUsageFlags::TRANSFER_DST,
                )
                .push_next(&mut profile_list);
            let mut format_count = 0;
            vk_result((video_instance
                .fp()
                .get_physical_device_video_format_properties_khr)(
                frame.physical_device,
                &format_info,
                &mut format_count,
                std::ptr::null_mut(),
            ))
            .map_err(|error| format!("count HEVC input formats: {error}"))?;
            if format_count == 0 {
                return Err("driver exposes no HEVC encode input format".into());
            }
            let mut formats = vec![vk::VideoFormatPropertiesKHR::default(); format_count as usize];
            vk_result((video_instance
                .fp()
                .get_physical_device_video_format_properties_khr)(
                frame.physical_device,
                &format_info,
                &mut format_count,
                formats.as_mut_ptr(),
            ))
            .map_err(|error| format!("query HEVC input formats: {error}"))?;
            let input_format = formats
                .iter()
                .find(|properties| properties.format == vk::Format::G8_B8R8_2PLANE_420_UNORM)
                .ok_or("driver does not expose two-plane NV12 HEVC input")?
                .format;

            let mut hevc_session = vk::VideoEncodeH265SessionCreateInfoKHR::default()
                .use_max_level_idc(true)
                .max_level_idc(native::StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_5_1);
            let session_info = vk::VideoSessionCreateInfoKHR::default()
                .queue_family_index(frame.queue_family)
                .video_profile(&profile)
                .picture_format(input_format)
                .max_coded_extent(extent)
                .reference_picture_format(input_format)
                .max_dpb_slots(1)
                .max_active_reference_pictures(0)
                .std_header_version(&std_header_version)
                .push_next(&mut hevc_session);
            let mut session = vk::VideoSessionKHR::null();
            vk_result((video.fp().create_video_session_khr)(
                frame.device.handle(),
                &session_info,
                std::ptr::null(),
                &mut session,
            ))
            .map_err(|error| format!("create HEVC video session: {error}"))?;
            let session_memory = bind_session_memory(frame, &video, session)?;

            let blocks = HevcBlockSizes::from_capabilities(&hevc_capabilities)?;
            let (profile_tier, mut vps, mut sps, pps) =
                hevc_parameter_structures(width, height, blocks);
            let dec_pic_buf: native::StdVideoH265DecPicBufMgr = std::mem::zeroed();
            vps.pProfileTierLevel = &raw const profile_tier;
            vps.pDecPicBufMgr = &raw const dec_pic_buf;
            sps.pProfileTierLevel = &raw const profile_tier;
            sps.pDecPicBufMgr = &raw const dec_pic_buf;
            let vps_list = [vps];
            let sps_list = [sps];
            let pps_list = [pps];
            let add = vk::VideoEncodeH265SessionParametersAddInfoKHR::default()
                .std_vp_ss(&vps_list)
                .std_sp_ss(&sps_list)
                .std_pp_ss(&pps_list);
            let mut hevc_parameters = vk::VideoEncodeH265SessionParametersCreateInfoKHR::default()
                .max_std_vps_count(1)
                .max_std_sps_count(1)
                .max_std_pps_count(1)
                .parameters_add_info(&add);
            let parameters_info = vk::VideoSessionParametersCreateInfoKHR::default()
                .video_session(session)
                .push_next(&mut hevc_parameters);
            let mut parameters = vk::VideoSessionParametersKHR::null();
            vk_result((video.fp().create_video_session_parameters_khr)(
                frame.device.handle(),
                &parameters_info,
                std::ptr::null(),
                &mut parameters,
            ))
            .map_err(|error| format!("create HEVC session parameters: {error}"))?;
            let parameter_sets = encoded_hevc_parameter_sets(&encode, &frame.device, parameters)
                .map_err(|error| format!("encode HEVC parameter sets: {error}"))?;
            let input = create_video_image(
                frame,
                width,
                height,
                input_format,
                vk::ImageUsageFlags::VIDEO_ENCODE_SRC_KHR | vk::ImageUsageFlags::TRANSFER_DST,
                &profiles,
            )
            .map_err(|error| format!("create HEVC input image: {error}"))?;
            let dpb = create_video_image(
                frame,
                width,
                height,
                input_format,
                vk::ImageUsageFlags::VIDEO_ENCODE_DPB_KHR,
                &profiles,
            )
            .map_err(|error| format!("create HEVC reconstructed image: {error}"))?;
            let bitstream_offset =
                (parameter_sets.len() as u64).next_multiple_of(bitstream_offset_alignment);
            let bitstream_size = (bitstream_offset + width as u64 * height as u64 * 3 + 64 * 1024)
                .next_multiple_of(bitstream_alignment);
            let bitstream = create_video_buffer(frame, bitstream_size, &profiles)
                .map_err(|error| format!("create HEVC bitstream buffer: {error}"))?;
            let mut feedback = vk::QueryPoolVideoEncodeFeedbackCreateInfoKHR::default()
                .encode_feedback_flags(vk::VideoEncodeFeedbackFlagsKHR::BITSTREAM_BYTES_WRITTEN);
            let mut query_profile = profile;
            let query_info = vk::QueryPoolCreateInfo::default()
                .query_type(vk::QueryType::VIDEO_ENCODE_FEEDBACK_KHR)
                .query_count(1)
                .push_next(&mut query_profile)
                .push_next(&mut feedback);
            let query_pool = frame.device.create_query_pool(&query_info, None)?;
            let command_pool = frame.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                    .queue_family_index(frame.queue_family),
                None,
            )?;
            let command = frame.device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )?[0];
            let fence = frame.device.create_fence(
                &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                None,
            )?;

            Ok(Self {
                codec: Codec::Hevc,
                dpb: Some(dpb),
                device: frame.device.clone(),
                video,
                encode,
                session,
                parameters,
                session_memory,
                input,
                parameter_sets,
                queue: frame.queue,
                command_pool,
                command,
                fence,
                fence_in_flight: false,
                bitstream,
                bitstream_offset,
                query_pool,
                width,
                height,
                bitrate: (bitrate as u64).min(encode_capabilities.max_bitrate),
                frame_rate: frame_rate.round().max(1.0) as u32,
                rate_control_mode,
                initialized: false,
            })
        }
    }
}

pub(super) fn preferred_rate_control(
    capabilities: &vk::VideoEncodeCapabilitiesKHR<'_>,
) -> vk::VideoEncodeRateControlModeFlagsKHR {
    if capabilities
        .rate_control_modes
        .contains(vk::VideoEncodeRateControlModeFlagsKHR::CBR)
    {
        vk::VideoEncodeRateControlModeFlagsKHR::CBR
    } else if capabilities
        .rate_control_modes
        .contains(vk::VideoEncodeRateControlModeFlagsKHR::VBR)
    {
        vk::VideoEncodeRateControlModeFlagsKHR::VBR
    } else {
        vk::VideoEncodeRateControlModeFlagsKHR::DISABLED
    }
}
