mod native;
use super::VulkanFrame;
use parking_lot::Mutex;
use polymer_renderer::webrtc::{Codec, EncodedFrame, VideoFrame};
use std::sync::Arc;
#[derive(Default)]
pub(crate) struct Session {
    native: Option<native::Encoder>,
    requested: Option<Codec>,
    failed: bool,
}
impl Session {
    pub fn frame(
        session: Arc<Mutex<Self>>,
        connection: u64,
        width: u32,
        height: u32,
        mut frame: VulkanFrame,
    ) -> VideoFrame {
        VideoFrame::new(
            connection,
            width,
            height,
            move |width, height, preferred, bitrate, rate| {
                session
                    .lock()
                    .encode(&mut frame, width, height, preferred, bitrate, rate)
            },
        )
    }
    fn encode(
        &mut self,
        frame: &mut VulkanFrame,
        width: u32,
        height: u32,
        preferred: Codec,
        bitrate: u32,
        rate: f32,
    ) -> Result<EncodedFrame, Box<dyn std::error::Error + Send + Sync>> {
        if self.failed {
            return Err("hardware encoding disabled after GPU failure".into());
        }
        if self.requested != Some(preferred) {
            self.native = None;
            self.requested = Some(preferred);
        }
        if self.native.is_none() {
            self.native = Some(native::Encoder::new_for_frame(
                frame, width, height, bitrate, rate, preferred,
            )?);
        }
        let encoder = self.native.as_mut().unwrap();
        match encoder.encode_frame(frame, width, height) {
            Ok(data) => Ok(EncodedFrame {
                backend: "VULKAN Video Encode",
                codec: encoder.codec(),
                data,
            }),
            Err(error) => {
                if encoder.has_in_flight_work() {
                    let stalled = self.native.take().unwrap();
                    std::mem::forget(stalled);
                    self.failed = true;
                } else {
                    self.native = None;
                }
                Err(error)
            }
        }
    }
}
