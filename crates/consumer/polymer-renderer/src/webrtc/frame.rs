//! Backend-neutral encoded frame handoff. Native resources stay in backend-owned jobs.
use std::{collections::VecDeque, error::Error};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Codec {
    Hevc,
    H264,
}
impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Self::Hevc => "HEVC",
            Self::H264 => "H.264",
        }
    }
}
pub struct EncodedFrame {
    pub backend: &'static str,
    pub codec: Codec,
    pub data: Vec<u8>,
}
pub type EncodeError = Box<dyn Error + Send + Sync>;
type EncodeJob =
    Box<dyn FnOnce(u32, u32, Codec, u32, f32) -> Result<EncodedFrame, EncodeError> + Send>;
pub struct VideoFrame {
    pub connection: u64,
    pub width: u32,
    pub height: u32,
    encode: EncodeJob,
}
impl VideoFrame {
    pub fn new(
        connection: u64,
        width: u32,
        height: u32,
        encode: impl FnOnce(u32, u32, Codec, u32, f32) -> Result<EncodedFrame, EncodeError>
        + Send
        + 'static,
    ) -> Self {
        Self {
            connection,
            width,
            height,
            encode: Box::new(encode),
        }
    }
    pub(super) fn encode(
        self,
        codec: Codec,
        bitrate: u32,
        rate: f32,
    ) -> Result<EncodedFrame, EncodeError> {
        (self.encode)(self.width, self.height, codec, bitrate, rate)
    }
}
/// Bounded handoff; dropping a rejected job releases its backend resource lease.
pub struct VideoSwapchain {
    capacity: usize,
    frames: VecDeque<VideoFrame>,
}
impl VideoSwapchain {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            frames: VecDeque::new(),
        }
    }
    pub fn available(&self) -> usize {
        self.capacity.saturating_sub(self.frames.len())
    }
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
    pub fn present(&mut self, frame: VideoFrame) -> bool {
        if self.frames.len() >= self.capacity {
            return false;
        }
        self.frames.push_back(frame);
        true
    }
    pub fn disconnect_stream(&mut self, connection: u64) {
        self.frames.retain(|frame| frame.connection != connection);
    }
    pub fn clear(&mut self) {
        self.frames.clear();
    }
    pub fn drain(&mut self) -> Vec<VideoFrame> {
        self.frames.drain(..).collect()
    }
}
