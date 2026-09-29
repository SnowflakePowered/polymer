use super::*;

impl HevcBlockSizes {
    pub(super) fn from_capabilities(
        capabilities: &vk::VideoEncodeH265CapabilitiesKHR,
    ) -> Result<Self, Error> {
        let ctb_log2 = if capabilities
            .ctb_sizes
            .contains(vk::VideoEncodeH265CtbSizeFlagsKHR::TYPE_64)
        {
            6
        } else if capabilities
            .ctb_sizes
            .contains(vk::VideoEncodeH265CtbSizeFlagsKHR::TYPE_32)
        {
            5
        } else if capabilities
            .ctb_sizes
            .contains(vk::VideoEncodeH265CtbSizeFlagsKHR::TYPE_16)
        {
            4
        } else {
            return Err("driver reports no HEVC coding tree block size".into());
        };
        let tu_flags = [
            (vk::VideoEncodeH265TransformBlockSizeFlagsKHR::TYPE_4, 2),
            (vk::VideoEncodeH265TransformBlockSizeFlagsKHR::TYPE_8, 3),
            (vk::VideoEncodeH265TransformBlockSizeFlagsKHR::TYPE_16, 4),
            (vk::VideoEncodeH265TransformBlockSizeFlagsKHR::TYPE_32, 5),
        ];
        let min_tu_log2 = tu_flags
            .iter()
            .find(|(flag, _)| capabilities.transform_block_sizes.contains(*flag))
            .map(|(_, log2)| *log2)
            .ok_or("driver reports no HEVC transform block size")?;
        let max_tu_log2 = tu_flags
            .iter()
            .rev()
            .find(|(flag, _)| capabilities.transform_block_sizes.contains(*flag))
            .map(|(_, log2)| *log2)
            .ok_or("driver reports no HEVC transform block size")?;
        Ok(Self {
            ctb_log2,
            min_tu_log2,
            max_tu_log2,
        })
    }
}

pub(super) fn hevc_parameter_structures(
    width: u32,
    height: u32,
    blocks: HevcBlockSizes,
) -> (
    native::StdVideoH265ProfileTierLevel,
    native::StdVideoH265VideoParameterSet,
    native::StdVideoH265SequenceParameterSet,
    native::StdVideoH265PictureParameterSet,
) {
    let mut profile: native::StdVideoH265ProfileTierLevel = unsafe { std::mem::zeroed() };
    profile.flags.set_general_progressive_source_flag(1);
    profile.flags.set_general_frame_only_constraint_flag(1);
    profile.general_profile_idc = native::StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN;
    profile.general_level_idc = native::StdVideoH265LevelIdc_STD_VIDEO_H265_LEVEL_IDC_5_1;

    let mut vps: native::StdVideoH265VideoParameterSet = unsafe { std::mem::zeroed() };
    vps.flags.set_vps_temporal_id_nesting_flag(1);

    // pic_width/height_in_luma_samples must be multiples of MinCbSizeY (8);
    // the conformance window crops the alignment padding back off.
    let coded_width = width.div_ceil(8) * 8;
    let coded_height = height.div_ceil(8) * 8;
    let mut sps: native::StdVideoH265SequenceParameterSet = unsafe { std::mem::zeroed() };
    sps.flags.set_sps_temporal_id_nesting_flag(1);
    sps.flags.set_strong_intra_smoothing_enabled_flag(1);
    sps.chroma_format_idc =
        native::StdVideoH265ChromaFormatIdc_STD_VIDEO_H265_CHROMA_FORMAT_IDC_420;
    sps.pic_width_in_luma_samples = coded_width;
    sps.pic_height_in_luma_samples = coded_height;
    if coded_width != width || coded_height != height {
        sps.flags.set_conformance_window_flag(1);
        sps.conf_win_right_offset = (coded_width - width) / 2;
        sps.conf_win_bottom_offset = (coded_height - height) / 2;
    }
    sps.log2_max_pic_order_cnt_lsb_minus4 = 4;
    sps.log2_min_luma_coding_block_size_minus3 = 0;
    sps.log2_diff_max_min_luma_coding_block_size = (blocks.ctb_log2 - 3) as u8;
    sps.log2_min_luma_transform_block_size_minus2 = (blocks.min_tu_log2 - 2) as u8;
    sps.log2_diff_max_min_luma_transform_block_size =
        (blocks.max_tu_log2 - blocks.min_tu_log2) as u8;
    sps.max_transform_hierarchy_depth_inter = (blocks.ctb_log2 - blocks.min_tu_log2).min(3) as u8;
    sps.max_transform_hierarchy_depth_intra = (blocks.ctb_log2 - blocks.min_tu_log2).min(3) as u8;

    let mut pps: native::StdVideoH265PictureParameterSet = unsafe { std::mem::zeroed() };
    pps.flags.set_deblocking_filter_control_present_flag(1);
    pps.log2_parallel_merge_level_minus2 = 0;
    (profile, vps, sps, pps)
}

pub(super) fn h264_parameter_structures(
    width: u32,
    height: u32,
) -> (
    native::StdVideoH264SequenceParameterSet,
    native::StdVideoH264PictureParameterSet,
) {
    let coded_width = width.div_ceil(16) * 16;
    let coded_height = height.div_ceil(16) * 16;
    let mut sps: native::StdVideoH264SequenceParameterSet = unsafe { std::mem::zeroed() };
    sps.flags.set_frame_mbs_only_flag(1);
    sps.flags.set_direct_8x8_inference_flag(1);
    if coded_width != width || coded_height != height {
        sps.flags.set_frame_cropping_flag(1);
    }
    sps.profile_idc = native::StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_BASELINE;
    sps.level_idc = native::StdVideoH264LevelIdc_STD_VIDEO_H264_LEVEL_IDC_4_1;
    sps.chroma_format_idc =
        native::StdVideoH264ChromaFormatIdc_STD_VIDEO_H264_CHROMA_FORMAT_IDC_420;
    sps.log2_max_frame_num_minus4 = 0;
    sps.pic_order_cnt_type = native::StdVideoH264PocType_STD_VIDEO_H264_POC_TYPE_0;
    sps.log2_max_pic_order_cnt_lsb_minus4 = 0;
    sps.max_num_ref_frames = 0;
    sps.pic_width_in_mbs_minus1 = coded_width / 16 - 1;
    sps.pic_height_in_map_units_minus1 = coded_height / 16 - 1;
    sps.frame_crop_right_offset = (coded_width - width) / 2;
    sps.frame_crop_bottom_offset = (coded_height - height) / 2;

    let mut pps: native::StdVideoH264PictureParameterSet = unsafe { std::mem::zeroed() };
    pps.flags.set_entropy_coding_mode_flag(0);
    pps.flags.set_deblocking_filter_control_present_flag(1);
    (sps, pps)
}

pub(super) unsafe fn encoded_parameter_sets(
    encode: &ash::khr::video_encode_queue::Device,
    device: &ash::Device,
    parameters: vk::VideoSessionParametersKHR,
) -> Result<Vec<u8>, Error> {
    unsafe {
        let mut h264_get = vk::VideoEncodeH264SessionParametersGetInfoKHR::default()
            .write_std_sps(true)
            .write_std_pps(true)
            .std_sps_id(0)
            .std_pps_id(0);
        let get = vk::VideoEncodeSessionParametersGetInfoKHR::default()
            .video_session_parameters(parameters)
            .push_next(&mut h264_get);
        let mut size = 0;
        vk_result((encode.fp().get_encoded_video_session_parameters_khr)(
            device.handle(),
            &get,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
        ))?;
        let mut bytes = vec![0; size];
        vk_result((encode.fp().get_encoded_video_session_parameters_khr)(
            device.handle(),
            &get,
            std::ptr::null_mut(),
            &mut size,
            bytes.as_mut_ptr().cast(),
        ))?;
        bytes.truncate(size);
        Ok(bytes)
    }
}

pub(super) unsafe fn encoded_hevc_parameter_sets(
    encode: &ash::khr::video_encode_queue::Device,
    device: &ash::Device,
    parameters: vk::VideoSessionParametersKHR,
) -> Result<Vec<u8>, Error> {
    unsafe {
        let mut hevc_get = vk::VideoEncodeH265SessionParametersGetInfoKHR::default()
            .write_std_vps(true)
            .write_std_sps(true)
            .write_std_pps(true)
            .std_vps_id(0)
            .std_sps_id(0)
            .std_pps_id(0);
        let get = vk::VideoEncodeSessionParametersGetInfoKHR::default()
            .video_session_parameters(parameters)
            .push_next(&mut hevc_get);
        let mut size = 0;
        vk_result((encode.fp().get_encoded_video_session_parameters_khr)(
            device.handle(),
            &get,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
        ))?;
        let mut bytes = vec![0; size];
        vk_result((encode.fp().get_encoded_video_session_parameters_khr)(
            device.handle(),
            &get,
            std::ptr::null_mut(),
            &mut size,
            bytes.as_mut_ptr().cast(),
        ))?;
        bytes.truncate(size);
        Ok(bytes)
    }
}
