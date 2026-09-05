//! Contracts implemented by graphics producers and consumers.

use crate::{ExternalImageDesc, FrameReady, StreamId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceDesc {
    pub width: u32,
    pub height: u32,
    pub format: u32,
}

#[derive(Debug)]
pub enum CaptureError {
    Unsupported(&'static str),
    DeviceLost,
    Backend(String),
}

pub trait FrameProducer {
    fn stream_id(&self) -> StreamId;
    fn exported_images(&self) -> &[ExternalImageDesc];
    fn recreate(&mut self, surface: SurfaceDesc) -> Result<(), CaptureError>;
}

pub trait FrameConsumer {
    fn import_images(&mut self, images: &[ExternalImageDesc]) -> Result<(), CaptureError>;
    fn consume(&mut self, frame: FrameReady) -> Result<(), CaptureError>;
}
