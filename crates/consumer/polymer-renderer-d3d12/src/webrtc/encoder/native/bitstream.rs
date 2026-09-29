use super::*;

/// Builds the VPS/SPS/PPS that D3D12 intentionally leaves to the host. The
/// values mirror the all-intra Main-profile configuration submitted below.
/// Writes Annex-B VPS/SPS/PPS matching the D3D12 encoder configuration. The
/// encoder emits only slice data, so every SPS field that the hardware bakes
/// into slices (coding-unit range, transform range, AMP) must agree with
/// `support.configuration`, and the picture dimensions must be aligned to the
/// subregion block size with a conformance window covering the padding.
pub(super) fn hevc_parameter_sets(width: u32, height: u32, support: &HevcSupport) -> Vec<u8> {
    let configuration = &support.configuration;
    let min_cu_log2 = configuration.MinLumaCodingUnitSize.0 as u32 + 3;
    let max_cu_log2 = configuration.MaxLumaCodingUnitSize.0 as u32 + 3;
    let min_tu_log2 = configuration.MinLumaTransformUnitSize.0 as u32 + 2;
    let max_tu_log2 = configuration.MaxLumaTransformUnitSize.0 as u32 + 2;
    let amp_enabled = configuration.ConfigurationFlags.0
        & D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_HEVC_FLAG_USE_ASYMETRIC_MOTION_PARTITION.0
        != 0;
    let alignment = support.subregion_block_size.max(1 << min_cu_log2) as u64;
    let coded_width = (width as u64).next_multiple_of(alignment) as u32;
    let coded_height = (height as u64).next_multiple_of(alignment) as u32;
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

pub(super) fn hevc_profile_tier_level(bits: &mut BitWriter) {
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

pub(super) fn append_hevc_nal(output: &mut Vec<u8>, nal_type: u8, rbsp: Vec<u8>) {
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
pub(super) struct BitWriter {
    bytes: Vec<u8>,
    current: u8,
    used: u8,
}

impl BitWriter {
    // Matches the Main/4.1, CAVLC, all-IDR configuration passed to D3D12.
    // https://microsoft.github.io/DirectX-Specs/d3d/D3D12_Video_Encoding_H264_HEVC.html#h264-sequence-parameter-set-expected-values
    pub(super) fn h264_parameter_sets(width: u32, height: u32) -> Vec<u8> {
        let mut sps = Self::default();
        sps.bits(77, 8); // Main profile
        sps.bits(0, 8); // constraint flags
        sps.bits(41, 8); // level 4.1
        sps.ue(0); // sequence parameter set ID
        sps.ue(11); // log2_max_frame_num_minus4, matching the GOP
        sps.ue(2); // pic_order_cnt_type
        sps.ue(1); // max_num_ref_frames
        sps.bit(false); // gaps_in_frame_num_value_allowed_flag
        sps.ue(width.div_ceil(16) - 1);
        sps.ue(height.div_ceil(16) - 1);
        sps.bit(true); // frame_mbs_only_flag
        sps.bit(true); // direct_8x8_inference_flag
        let cropped = width % 16 != 0 || height % 16 != 0;
        sps.bit(cropped);
        if cropped {
            sps.ue(0);
            sps.ue((width.div_ceil(16) * 16 - width) / 2);
            sps.ue(0);
            sps.ue((height.div_ceil(16) * 16 - height) / 2);
        }
        sps.bit(false); // vui_parameters_present_flag

        let mut pps = Self::default();
        pps.ue(0); // picture parameter set ID
        pps.ue(0); // sequence parameter set ID
        pps.bit(false); // CAVLC
        pps.bit(false); // bottom_field_pic_order_in_frame_present_flag
        pps.ue(0); // num_slice_groups_minus1
        pps.ue(0); // num_ref_idx_l0_default_active_minus1
        pps.ue(0); // num_ref_idx_l1_default_active_minus1
        pps.bit(false); // weighted_pred_flag
        pps.bits(0, 2); // weighted_bipred_idc
        pps.se(0); // pic_init_qp_minus26
        pps.se(0); // pic_init_qs_minus26
        pps.se(0); // chroma_qp_index_offset
        pps.bit(true); // deblocking_filter_control_present_flag
        pps.bit(false); // constrained_intra_pred_flag
        pps.bit(false); // redundant_pic_cnt_present_flag
        let mut headers = Vec::new();
        sps.append_h264_nal(&mut headers, 0x67);
        pps.append_h264_nal(&mut headers, 0x68);
        headers
    }

    pub(super) fn append_h264_nal(self, output: &mut Vec<u8>, header: u8) {
        output.extend_from_slice(&[0, 0, 0, 1, header]);
        let mut zeros = 0;
        for byte in self.finish() {
            if zeros >= 2 && byte <= 3 {
                output.push(3);
                zeros = 0;
            }
            output.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
    }

    pub(super) fn bit(&mut self, value: bool) {
        self.current |= u8::from(value) << (7 - self.used);
        self.used += 1;
        if self.used == 8 {
            self.bytes.push(self.current);
            self.current = 0;
            self.used = 0;
        }
    }

    pub(super) fn bits(&mut self, value: u64, count: u8) {
        for shift in (0..count).rev() {
            self.bit((value >> shift) & 1 != 0);
        }
    }

    pub(super) fn ue(&mut self, value: u32) {
        let code = value as u64 + 1;
        let width = 64 - code.leading_zeros() as u8;
        self.bits(0, width - 1);
        self.bits(code, width);
    }

    pub(super) fn se(&mut self, value: i32) {
        let code = if value <= 0 {
            value.unsigned_abs() * 2
        } else {
            value as u32 * 2 - 1
        };
        self.ue(code);
    }

    pub(super) fn finish(mut self) -> Vec<u8> {
        self.bit(true);
        while self.used != 0 {
            self.bit(false);
        }
        self.bytes
    }
}
