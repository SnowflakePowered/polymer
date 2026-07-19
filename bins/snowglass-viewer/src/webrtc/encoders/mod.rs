//! H.264 encoder selection. D3D12 Video Encode is preferred; OpenH264 is the fallback.

mod d3d12;
mod openh264;
mod vulkan;

use super::{VideoFrame, VideoFrameData};

pub(super) struct Encoder {
    d3d12: Option<d3d12::Encoder>,
    d3d12_disabled: bool,
    d3d12_announced: bool,
    vulkan: Option<vulkan::Encoder>,
    vulkan_disabled: bool,
    vulkan_announced: bool,
    openh264: Option<openh264::Encoder>,
    bitrate: u32,
    frame_rate: f32,
}

impl Encoder {
    pub(super) fn new(bitrate: u32, frame_rate: f32) -> Self {
        Self {
            d3d12: None,
            d3d12_disabled: false,
            d3d12_announced: false,
            vulkan: None,
            vulkan_disabled: false,
            vulkan_announced: false,
            openh264: None,
            bitrate,
            frame_rate,
        }
    }

    pub(super) fn encode(
        &mut self,
        mut frame: VideoFrame,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        let width = frame.width;
        let height = frame.height;
        if let VideoFrameData::D3d12(gpu) = &mut frame.data {
            if self.d3d12.is_none() && !self.d3d12_disabled {
                match d3d12::Encoder::new_for_frame(
                    gpu,
                    width,
                    height,
                    self.bitrate,
                    self.frame_rate,
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
                            eprintln!("snowglass: using D3D12 Video Encode for {width}x{height}");
                            self.d3d12_announced = true;
                        }
                        return Ok(encoded);
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
            if self.vulkan.is_none() && !self.vulkan_disabled {
                match vulkan::Encoder::new_for_frame(
                    gpu,
                    width,
                    height,
                    self.bitrate,
                    self.frame_rate,
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
                            eprintln!("snowglass: using Vulkan Video Encode for {width}x{height}");
                            self.vulkan_announced = true;
                        }
                        return Ok(encoded);
                    }
                    Err(error) => {
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
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        let encoder = self.openh264.get_or_insert_with(|| {
            openh264::Encoder::new(self.bitrate, self.frame_rate)
                .expect("OpenH264 encoder initialization failed")
        });
        encoder.encode(rgba, width, height)
    }
}
