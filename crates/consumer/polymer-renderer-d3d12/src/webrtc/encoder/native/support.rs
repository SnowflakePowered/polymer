use super::*;

pub(super) unsafe fn query_codec_support(
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

pub(super) unsafe fn query_hevc_support(
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
            metadata_size: (requirements1.max_encoder_output_metadata_buffer_size as u64)
                .next_multiple_of(
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

pub(super) unsafe fn query_h264_support(
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
            metadata_size: (requirements1.max_encoder_output_metadata_buffer_size as u64)
                .next_multiple_of(
                    requirements1
                        .encoder_metadata_buffer_access_alignment
                        .max(1) as u64,
                ),
            hevc: None,
        })
    }
}

pub(super) unsafe fn check_video_feature<T>(
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

pub(super) fn profile_desc(
    profile: &mut D3D12_VIDEO_ENCODER_PROFILE_H264,
) -> D3D12_VIDEO_ENCODER_PROFILE_DESC {
    D3D12_VIDEO_ENCODER_PROFILE_DESC {
        DataSize: size_of::<D3D12_VIDEO_ENCODER_PROFILE_H264>() as u32,
        Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 {
            pH264Profile: profile,
        },
    }
}

pub(super) fn hevc_profile_desc(
    profile: &mut D3D12_VIDEO_ENCODER_PROFILE_HEVC,
) -> D3D12_VIDEO_ENCODER_PROFILE_DESC {
    D3D12_VIDEO_ENCODER_PROFILE_DESC {
        DataSize: size_of::<D3D12_VIDEO_ENCODER_PROFILE_HEVC>() as u32,
        Anonymous: D3D12_VIDEO_ENCODER_PROFILE_DESC_0 {
            pHEVCProfile: profile,
        },
    }
}

pub(super) fn hevc_level_desc(
    level: &mut D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC,
) -> D3D12_VIDEO_ENCODER_LEVEL_SETTING {
    D3D12_VIDEO_ENCODER_LEVEL_SETTING {
        DataSize: size_of::<D3D12_VIDEO_ENCODER_LEVEL_TIER_CONSTRAINTS_HEVC>() as u32,
        Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 {
            pHEVCLevelSetting: level,
        },
    }
}

pub(super) fn h264_configuration() -> D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264 {
    D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264 {
        ConfigurationFlags: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_FLAG_NONE,
        DirectModeConfig: D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_DIRECT_MODES_DISABLED,
        DisableDeblockingFilterConfig:
            D3D12_VIDEO_ENCODER_CODEC_CONFIGURATION_H264_SLICES_DEBLOCKING_MODE_0_ALL_LUMA_CHROMA_SLICE_BLOCK_EDGES_ALWAYS_FILTERED,
    }
}

pub(super) fn level_desc(
    level: &mut D3D12_VIDEO_ENCODER_LEVELS_H264,
) -> D3D12_VIDEO_ENCODER_LEVEL_SETTING {
    D3D12_VIDEO_ENCODER_LEVEL_SETTING {
        DataSize: size_of::<D3D12_VIDEO_ENCODER_LEVELS_H264>() as u32,
        Anonymous: D3D12_VIDEO_ENCODER_LEVEL_SETTING_0 {
            pH264LevelSetting: level,
        },
    }
}
