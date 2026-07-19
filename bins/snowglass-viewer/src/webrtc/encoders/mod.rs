//! HEVC/H.264 encoder selection. Native HEVC is preferred, followed by native
//! H.264 and finally OpenH264.

mod d3d12;
mod openh264;
mod vulkan;

use super::{VideoFrame, VideoFrameData};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Codec {
    Hevc,
    H264,
}

impl Codec {
    fn name(self) -> &'static str {
        match self {
            Self::Hevc => "HEVC",
            Self::H264 => "H.264",
        }
    }
}

pub(super) struct EncodedFrame {
    pub(super) codec: Codec,
    pub(super) data: Vec<u8>,
}

pub(super) struct Encoder {
    d3d12: Option<d3d12::Encoder>,
    d3d12_disabled: bool,
    d3d12_announced: bool,
    vulkan: Option<vulkan::Encoder>,
    vulkan_disabled: bool,
    vulkan_gpu_stalled: bool,
    vulkan_announced: bool,
    openh264: Option<openh264::Encoder>,
    openh264_announced: bool,
    bitrate: u32,
    frame_rate: f32,
    preferred: Option<Codec>,
}

impl Encoder {
    pub(super) fn new(bitrate: u32, frame_rate: f32) -> Self {
        Self {
            d3d12: None,
            d3d12_disabled: false,
            d3d12_announced: false,
            vulkan: None,
            vulkan_disabled: false,
            vulkan_gpu_stalled: false,
            vulkan_announced: false,
            openh264: None,
            openh264_announced: false,
            bitrate,
            frame_rate,
            preferred: None,
        }
    }

    pub(super) fn encode(
        &mut self,
        mut frame: VideoFrame,
        preferred: Codec,
    ) -> Result<EncodedFrame, Box<dyn std::error::Error + Send + Sync>> {
        if self.preferred != Some(preferred) {
            self.d3d12 = None;
            self.d3d12_disabled = false;
            if !self.vulkan_gpu_stalled {
                self.vulkan = None;
                self.vulkan_disabled = false;
            }
            self.preferred = Some(preferred);
        }
        let width = frame.width;
        let height = frame.height;
        if let VideoFrameData::D3d12(gpu) = &mut frame.data {
            if self
                .d3d12
                .as_ref()
                .is_some_and(|encoder| encoder.codec() != preferred)
            {
                self.d3d12 = None;
                self.d3d12_disabled = false;
                self.d3d12_announced = false;
            }
            if self.d3d12.is_none() && !self.d3d12_disabled {
                match d3d12::Encoder::new_for_frame(
                    gpu,
                    width,
                    height,
                    self.bitrate,
                    self.frame_rate,
                    preferred,
                ) {
                    Ok(encoder) => self.d3d12 = Some(encoder),
                    Err(error) => {
                        eprintln!(
                            "snowglass: D3D12 Video Encode unavailable; using OpenH264: {error}"
                        );
                        self.d3d12_disabled = true;
                    }
                }
            }
            if let Some(encoder) = &mut self.d3d12 {
                match encoder.encode_frame(gpu, width, height) {
                    Ok(encoded) => {
                        if !self.d3d12_announced {
                            eprintln!(
                                "snowglass: using D3D12 Video Encode {} for {width}x{height}",
                                encoder.codec().name()
                            );
                            self.d3d12_announced = true;
                        }
                        return Ok(EncodedFrame {
                            codec: encoder.codec(),
                            data: encoded,
                        });
                    }
                    Err(error) => {
                        eprintln!("snowglass: D3D12 Video Encode failed; using OpenH264: {error}");
                        self.d3d12 = None;
                        self.d3d12_disabled = true;
                    }
                }
            }
            let rgba = d3d12::readback_rgba(gpu, width, height)?;
            return self.encode_openh264(&rgba, width, height);
        }
        if let VideoFrameData::Vulkan(gpu) = &mut frame.data {
            if self
                .vulkan
                .as_ref()
                .is_some_and(|encoder| encoder.codec() != preferred)
            {
                self.vulkan = None;
                self.vulkan_disabled = false;
                self.vulkan_announced = false;
            }
            if self.vulkan_gpu_stalled {
                return Err("Vulkan WebRTC encoding disabled after a GPU timeout".into());
            }
            if self.vulkan.is_none() && !self.vulkan_disabled {
                match vulkan::Encoder::new_for_frame(
                    gpu,
                    width,
                    height,
                    self.bitrate,
                    self.frame_rate,
                    preferred,
                ) {
                    Ok(encoder) => self.vulkan = Some(encoder),
                    Err(error) => {
                        eprintln!(
                            "snowglass: Vulkan Video Encode unavailable; using OpenH264: {error}"
                        );
                        self.vulkan_disabled = true;
                    }
                }
            }
            if let Some(encoder) = &mut self.vulkan {
                match encoder.encode_frame(gpu, width, height) {
                    Ok(encoded) => {
                        if !self.vulkan_announced {
                            eprintln!(
                                "snowglass: using Vulkan Video Encode {} for {width}x{height}",
                                encoder.codec().name()
                            );
                            self.vulkan_announced = true;
                        }
                        return Ok(EncodedFrame {
                            codec: encoder.codec(),
                            data: encoded,
                        });
                    }
                    Err(error) => {
                        if encoder.has_in_flight_work() {
                            eprintln!(
                                "snowglass: Vulkan Video Encode GPU timeout; disabling WebRTC encoding for this stream: {error}"
                            );
                            // Destroying objects referenced by a stalled queue is invalid and
                            // attempting a readback on that queue would hang too. Leak this one
                            // encoder until device teardown and keep dropping/releasing frames.
                            let stalled = self.vulkan.take().expect("Vulkan encoder is present");
                            std::mem::forget(stalled);
                            self.vulkan_disabled = true;
                            self.vulkan_gpu_stalled = true;
                            return Err(error);
                        }
                        eprintln!("snowglass: Vulkan Video Encode failed; using OpenH264: {error}");
                        self.vulkan = None;
                        self.vulkan_disabled = true;
                        if gpu.completion_submitted() {
                            return Err(error);
                        }
                    }
                }
            }
            let rgba = vulkan::readback_rgba(gpu, width, height)?;
            return self.encode_openh264(&rgba, width, height);
        }

        let VideoFrameData::Rgba(rgba) = frame.data else {
            unreachable!()
        };
        self.encode_openh264(&rgba, width, height)
    }

    fn encode_openh264(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> Result<EncodedFrame, Box<dyn std::error::Error + Send + Sync>> {
        let encoder = self.openh264.get_or_insert_with(|| {
            openh264::Encoder::new(self.bitrate, self.frame_rate)
                .expect("OpenH264 encoder initialization failed")
        });
        let encoded = encoder.encode(rgba, width, height)?;
        if !self.openh264_announced {
            eprintln!("snowglass: using OpenH264 for {width}x{height}");
            self.openh264_announced = true;
        }
        Ok(EncodedFrame {
            codec: Codec::H264,
            data: encoded,
        })
    }
}
