use super::*;

impl Encoder {
    pub(super) unsafe fn submit_bitstream_clear(&mut self) -> Result<(), Error> {
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

    pub(super) unsafe fn submit_crop(&mut self, frame: &mut D3d12Frame) -> Result<(), Error> {
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

    pub(super) unsafe fn submit_conversion(&mut self, frame: &mut D3d12Frame) -> Result<(), Error> {
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

    pub(super) unsafe fn submit_encode(&mut self) -> Result<(), Error> {
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

    pub(super) unsafe fn submit_hevc_encode(&mut self) -> Result<(), Error> {
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

    pub(super) unsafe fn submit_readback(&mut self) -> Result<(), Error> {
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

    pub(super) unsafe fn read_bitstream(&self) -> Result<Vec<u8>, Error> {
        unsafe {
            self.wait_readback()?;
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

/// Bounded wait: an encode pipeline that wedged (or a device that was
/// removed) must surface as an error instead of freezing the encoder thread.
pub(super) const GPU_WAIT_TIMEOUT_MS: u32 = 5_000;

impl Encoder {
    pub(super) fn wait_readback(&self) -> Result<(), Error> {
        if self
            .fence_waiter
            .wait(&self.fence, self.fence_value, GPU_WAIT_TIMEOUT_MS)?
        {
            Ok(())
        } else {
            Err("timed out waiting for D3D12 video fence".into())
        }
    }
}
