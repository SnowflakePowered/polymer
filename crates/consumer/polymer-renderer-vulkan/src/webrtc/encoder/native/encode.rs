use super::*;

impl VideoState {
    pub(super) fn encode_frame(
        &mut self,
        device: &ash::Device,
        frame: &mut VulkanFrame,
        conversion_done: vk::Semaphore,
        conversion_value: u64,
    ) -> Result<Vec<u8>, Error> {
        if self.codec == Codec::Hevc {
            return self.encode_hevc_frame(device, frame, conversion_done, conversion_value);
        }
        unsafe {
            self.begin_frame(device, "previous Vulkan video encode")?;
            self.record_h264_encode(device)?;
            self.submit_encode(device, frame, conversion_done, conversion_value)?;
            if !self.initialized {
                tracing::debug!(
                    "polymer: Vulkan Video Encode waiting for conversion timeline value {conversion_value}"
                );
            }
            self.wait_encode(device, frame, "Vulkan Video Encode")?;
            if !self.initialized {
                tracing::debug!("polymer: Vulkan Video Encode completed");
            }
            self.initialized = true;
            let encoded_size = self.h264_encoded_size(device)?;
            self.read_encoded(
                device,
                encoded_size,
                "Vulkan encoded size overflow",
                "Vulkan encoder returned an invalid bitstream size",
            )
        }
    }

    pub(super) fn encode_hevc_frame(
        &mut self,
        device: &ash::Device,
        frame: &mut VulkanFrame,
        conversion_done: vk::Semaphore,
        conversion_value: u64,
    ) -> Result<Vec<u8>, Error> {
        unsafe {
            self.begin_frame(device, "previous Vulkan HEVC encode")?;
            self.record_hevc_encode(device)?;
            if !self.initialized {
                tracing::debug!("polymer: Vulkan HEVC encode submitting first frame");
            }
            self.submit_encode(device, frame, conversion_done, conversion_value)?;
            self.wait_encode(device, frame, "Vulkan HEVC Encode")?;
            if !self.initialized {
                tracing::debug!("polymer: Vulkan HEVC encode first frame completed");
            }
            self.initialized = true;
            let encoded_size = self.hevc_encoded_size(device)?;
            self.read_encoded(
                device,
                encoded_size,
                "Vulkan HEVC encoded size overflow",
                "Vulkan HEVC encoder returned an invalid bitstream size",
            )
        }
    }

    pub(super) unsafe fn begin_frame(
        &mut self,
        device: &ash::Device,
        previous: &str,
    ) -> Result<(), Error> {
        unsafe {
            if self.fence_in_flight {
                wait_for_fence(device, self.fence, previous)?;
                self.fence_in_flight = false;
            }
            device.reset_fences(&[self.fence])?;
            device.reset_command_pool(self.command_pool, vk::CommandPoolResetFlags::empty())?;
            let mapped = device.map_memory(
                self.bitstream.memory,
                0,
                self.bitstream.size,
                vk::MemoryMapFlags::empty(),
            )?;
            std::ptr::copy_nonoverlapping(
                self.parameter_sets.as_ptr(),
                mapped.cast::<u8>(),
                self.parameter_sets.len(),
            );
            device.unmap_memory(self.bitstream.memory);
            device.begin_command_buffer(
                self.command,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;
            device.cmd_reset_query_pool(self.command, self.query_pool, 0, 1);
        }
        Ok(())
    }

    pub(super) fn constant_qp(&self) -> i32 {
        if self.rate_control_mode == vk::VideoEncodeRateControlModeFlagsKHR::DISABLED {
            26
        } else {
            0
        }
    }

    pub(super) unsafe fn record_h264_encode(&mut self, device: &ash::Device) -> Result<(), Error> {
        unsafe {
            let mut h264_rate_control = vk::VideoEncodeH264RateControlInfoKHR::default()
                .gop_frame_count(1)
                .idr_period(1)
                .consecutive_b_frame_count(0)
                .temporal_layer_count(1);
            let mut h264_rate_control_layer = vk::VideoEncodeH264RateControlLayerInfoKHR::default();
            let rate_control_layers = [vk::VideoEncodeRateControlLayerInfoKHR::default()
                .average_bitrate(self.bitrate)
                .max_bitrate(self.bitrate)
                .frame_rate_numerator(self.frame_rate)
                .frame_rate_denominator(1)
                .push_next(&mut h264_rate_control_layer)];
            let mut rate_control = vk::VideoEncodeRateControlInfoKHR::default()
                .rate_control_mode(self.rate_control_mode)
                .layers(
                    if self.rate_control_mode == vk::VideoEncodeRateControlModeFlagsKHR::DISABLED {
                        &[]
                    } else {
                        &rate_control_layers
                    },
                )
                .virtual_buffer_size_in_ms(1000)
                .initial_virtual_buffer_size_in_ms(500);
            let mut begin = vk::VideoBeginCodingInfoKHR::default()
                .video_session(self.session)
                .video_session_parameters(self.parameters);
            if self.initialized {
                begin = begin
                    .push_next(&mut rate_control)
                    .push_next(&mut h264_rate_control);
            }
            (self.video.fp().cmd_begin_video_coding_khr)(self.command, &begin);
            if !self.initialized {
                let control_flags = vk::VideoCodingControlFlagsKHR::RESET
                    | vk::VideoCodingControlFlagsKHR::ENCODE_RATE_CONTROL;
                let control = vk::VideoCodingControlInfoKHR::default()
                    .flags(control_flags)
                    .push_next(&mut rate_control)
                    .push_next(&mut h264_rate_control);
                (self.video.fp().cmd_control_video_coding_khr)(self.command, &control);
            }
            device.cmd_begin_query(
                self.command,
                self.query_pool,
                0,
                vk::QueryControlFlags::empty(),
            );

            let mut picture: native::StdVideoEncodeH264PictureInfo = std::mem::zeroed();
            picture.flags.set_IdrPicFlag(1);
            picture.seq_parameter_set_id = 0;
            picture.pic_parameter_set_id = 0;
            picture.idr_pic_id = 0;
            picture.primary_pic_type =
                native::StdVideoH264PictureType_STD_VIDEO_H264_PICTURE_TYPE_IDR;
            let mut slice: native::StdVideoEncodeH264SliceHeader = std::mem::zeroed();
            slice.slice_type = native::StdVideoH264SliceType_STD_VIDEO_H264_SLICE_TYPE_I;
            slice.cabac_init_idc = native::StdVideoH264CabacInitIdc_STD_VIDEO_H264_CABAC_INIT_IDC_0;
            slice.disable_deblocking_filter_idc = native::StdVideoH264DisableDeblockingFilterIdc_STD_VIDEO_H264_DISABLE_DEBLOCKING_FILTER_IDC_DISABLED;
            let slices = [vk::VideoEncodeH264NaluSliceInfoKHR::default()
                .constant_qp(self.constant_qp())
                .std_slice_header(&slice)];
            let mut h264_picture = vk::VideoEncodeH264PictureInfoKHR::default()
                .nalu_slice_entries(&slices)
                .std_picture_info(&picture);
            let picture_resource = vk::VideoPictureResourceInfoKHR::default()
                .coded_extent(vk::Extent2D {
                    width: self.width,
                    height: self.height,
                })
                .base_array_layer(0)
                .image_view_binding(self.input.view);
            let encode_info = vk::VideoEncodeInfoKHR::default()
                .dst_buffer(self.bitstream.buffer)
                .dst_buffer_offset(self.bitstream_offset)
                .dst_buffer_range(self.bitstream.size - self.bitstream_offset)
                .src_picture_resource(picture_resource)
                .preceding_externally_encoded_bytes(self.parameter_sets.len() as u32)
                .push_next(&mut h264_picture);
            (self.encode.fp().cmd_encode_video_khr)(self.command, &encode_info);
            device.cmd_end_query(self.command, self.query_pool, 0);
            let end = vk::VideoEndCodingInfoKHR::default();
            (self.video.fp().cmd_end_video_coding_khr)(self.command, &end);
            device.end_command_buffer(self.command)?;
        }
        Ok(())
    }

    pub(super) unsafe fn record_hevc_encode(&mut self, device: &ash::Device) -> Result<(), Error> {
        unsafe {
            let dpb = self.dpb.as_ref().expect("HEVC encoder has a DPB image");
            // The reconstructed picture is rewritten from scratch every frame
            // (IDR-only stream), so the previous contents can be discarded.
            let dpb_barrier = image_barrier()
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::VIDEO_ENCODE_DPB_KHR)
                .image(dpb.image)
                .subresource_range(color_range());
            device.cmd_pipeline_barrier(
                self.command,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[dpb_barrier],
            );
            let dpb_resource = vk::VideoPictureResourceInfoKHR::default()
                .coded_extent(vk::Extent2D {
                    width: self.width,
                    height: self.height,
                })
                .image_view_binding(dpb.view);
            let mut hevc_rate_control = vk::VideoEncodeH265RateControlInfoKHR::default()
                .gop_frame_count(1)
                .idr_period(1)
                .consecutive_b_frame_count(0)
                .sub_layer_count(1);
            let mut hevc_rate_control_layer = vk::VideoEncodeH265RateControlLayerInfoKHR::default();
            let rate_control_layers = [vk::VideoEncodeRateControlLayerInfoKHR::default()
                .average_bitrate(self.bitrate)
                .max_bitrate(self.bitrate)
                .frame_rate_numerator(self.frame_rate)
                .frame_rate_denominator(1)
                .push_next(&mut hevc_rate_control_layer)];
            let mut rate_control = vk::VideoEncodeRateControlInfoKHR::default()
                .rate_control_mode(self.rate_control_mode)
                .layers(
                    if self.rate_control_mode == vk::VideoEncodeRateControlModeFlagsKHR::DISABLED {
                        &[]
                    } else {
                        &rate_control_layers
                    },
                )
                .virtual_buffer_size_in_ms(1000)
                .initial_virtual_buffer_size_in_ms(500);
            // Bind the DPB resource without an active slot: every frame is an
            // IDR that re-activates slot 0 through the setup reference slot.
            let begin_slots = [vk::VideoReferenceSlotInfoKHR::default()
                .slot_index(-1)
                .picture_resource(&dpb_resource)];
            let mut begin = vk::VideoBeginCodingInfoKHR::default()
                .video_session(self.session)
                .video_session_parameters(self.parameters)
                .reference_slots(&begin_slots);
            if self.initialized {
                begin = begin
                    .push_next(&mut rate_control)
                    .push_next(&mut hevc_rate_control);
            }
            (self.video.fp().cmd_begin_video_coding_khr)(self.command, &begin);
            if !self.initialized {
                let control = vk::VideoCodingControlInfoKHR::default()
                    .flags(
                        vk::VideoCodingControlFlagsKHR::RESET
                            | vk::VideoCodingControlFlagsKHR::ENCODE_RATE_CONTROL,
                    )
                    .push_next(&mut rate_control)
                    .push_next(&mut hevc_rate_control);
                (self.video.fp().cmd_control_video_coding_khr)(self.command, &control);
            }
            device.cmd_begin_query(
                self.command,
                self.query_pool,
                0,
                vk::QueryControlFlags::empty(),
            );
            let mut picture: native::StdVideoEncodeH265PictureInfo = std::mem::zeroed();
            picture.flags.set_IrapPicFlag(1);
            picture.flags.set_is_reference(1);
            picture.pic_type = native::StdVideoH265PictureType_STD_VIDEO_H265_PICTURE_TYPE_IDR;
            let mut slice: native::StdVideoEncodeH265SliceSegmentHeader = std::mem::zeroed();
            slice.flags.set_first_slice_segment_in_pic_flag(1);
            slice.slice_type = native::StdVideoH265SliceType_STD_VIDEO_H265_SLICE_TYPE_I;
            slice.MaxNumMergeCand = 5;
            let slices = [vk::VideoEncodeH265NaluSliceSegmentInfoKHR::default()
                .constant_qp(self.constant_qp())
                .std_slice_segment_header(&slice)];
            let mut hevc_picture = vk::VideoEncodeH265PictureInfoKHR::default()
                .nalu_slice_segment_entries(&slices)
                .std_picture_info(&picture);
            let picture_resource = vk::VideoPictureResourceInfoKHR::default()
                .coded_extent(vk::Extent2D {
                    width: self.width,
                    height: self.height,
                })
                .image_view_binding(self.input.view);
            let mut reference: native::StdVideoEncodeH265ReferenceInfo = std::mem::zeroed();
            reference.pic_type = native::StdVideoH265PictureType_STD_VIDEO_H265_PICTURE_TYPE_IDR;
            let mut dpb_slot =
                vk::VideoEncodeH265DpbSlotInfoKHR::default().std_reference_info(&reference);
            let setup_slot = vk::VideoReferenceSlotInfoKHR::default()
                .slot_index(0)
                .picture_resource(&dpb_resource)
                .push_next(&mut dpb_slot);
            let encode_info = vk::VideoEncodeInfoKHR::default()
                .dst_buffer(self.bitstream.buffer)
                .dst_buffer_offset(self.bitstream_offset)
                .dst_buffer_range(self.bitstream.size - self.bitstream_offset)
                .src_picture_resource(picture_resource)
                .setup_reference_slot(&setup_slot)
                .preceding_externally_encoded_bytes(self.parameter_sets.len() as u32)
                .push_next(&mut hevc_picture);
            (self.encode.fp().cmd_encode_video_khr)(self.command, &encode_info);
            device.cmd_end_query(self.command, self.query_pool, 0);
            (self.video.fp().cmd_end_video_coding_khr)(
                self.command,
                &vk::VideoEndCodingInfoKHR::default(),
            );
            device.end_command_buffer(self.command)?;
        }
        Ok(())
    }

    pub(super) unsafe fn submit_encode(
        &mut self,
        device: &ash::Device,
        frame: &mut VulkanFrame,
        conversion_done: vk::Semaphore,
        conversion_value: u64,
    ) -> Result<(), Error> {
        let waits = [conversion_done];
        let wait_values = [conversion_value];
        let stages = [vk::PipelineStageFlags::ALL_COMMANDS];
        let signals = [frame.completion];
        let signal_values = [frame.completion_value];
        let commands = [self.command];
        let mut timeline = vk::TimelineSemaphoreSubmitInfo::default()
            .wait_semaphore_values(&wait_values)
            .signal_semaphore_values(&signal_values);
        let submit = vk::SubmitInfo::default()
            .push_next(&mut timeline)
            .wait_semaphores(&waits)
            .wait_dst_stage_mask(&stages)
            .command_buffers(&commands)
            .signal_semaphores(&signals);
        unsafe { device.queue_submit(self.queue, &[submit], self.fence)? };
        self.fence_in_flight = true;
        frame.mark_completion_submitted();
        Ok(())
    }

    pub(super) unsafe fn wait_encode(
        &mut self,
        device: &ash::Device,
        frame: &VulkanFrame,
        operation: &str,
    ) -> Result<(), Error> {
        if let Err(error) = wait_for_fence(device, self.fence, operation) {
            // Do not leave the compositor waiting forever if the driver
            // accepted an encode submission but never completes it.
            let _ = unsafe {
                device.signal_semaphore(
                    &vk::SemaphoreSignalInfo::default()
                        .semaphore(frame.completion)
                        .value(frame.completion_value),
                )
            };
            return Err(error);
        }
        self.fence_in_flight = false;
        Ok(())
    }

    pub(super) unsafe fn h264_encoded_size(&self, device: &ash::Device) -> Result<usize, Error> {
        let mut written = [0u64];
        unsafe {
            device.get_query_pool_results(
                self.query_pool,
                0,
                &mut written,
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WAIT,
            )?
        };
        Ok(written[0] as usize)
    }

    pub(super) unsafe fn hevc_encoded_size(&self, device: &ash::Device) -> Result<usize, Error> {
        // The encode fence has signaled, so the query must be resolvable.
        // WITH_STATUS instead of WAIT: a failed encode operation leaves
        // the feedback query unavailable and WAIT would block forever.
        let mut written = [[0u64; 2]];
        unsafe {
            device.get_query_pool_results(
                self.query_pool,
                0,
                &mut written,
                vk::QueryResultFlags::TYPE_64 | vk::QueryResultFlags::WITH_STATUS_KHR,
            )?
        };
        let status = written[0][1] as i64;
        if status <= 0 {
            return Err(format!("Vulkan HEVC encode feedback status {status}").into());
        }
        Ok(written[0][0] as usize)
    }

    pub(super) unsafe fn read_encoded(
        &self,
        device: &ash::Device,
        encoded_size: usize,
        overflow: &'static str,
        invalid: &'static str,
    ) -> Result<Vec<u8>, Error> {
        let encoded_end = (self.bitstream_offset as usize)
            .checked_add(encoded_size)
            .ok_or(overflow)?;
        if encoded_size == 0 || encoded_end > self.bitstream.size as usize {
            return Err(invalid.into());
        }
        unsafe {
            let mapped = device.map_memory(
                self.bitstream.memory,
                0,
                encoded_end as u64,
                vk::MemoryMapFlags::empty(),
            )?;
            let mut bytes = Vec::with_capacity(self.parameter_sets.len() + encoded_size);
            bytes.extend_from_slice(&self.parameter_sets);
            bytes.extend_from_slice(std::slice::from_raw_parts(
                mapped.cast::<u8>().add(self.bitstream_offset as usize),
                encoded_size,
            ));
            device.unmap_memory(self.bitstream.memory);
            Ok(bytes)
        }
    }
}
