//! Shared producer connection and publication. Native resource creation stays
//! in each graphics backend.
mod descriptor;
mod export;
mod failure;
mod frame;
mod lifecycle;
mod session;
#[cfg(feature = "test-support")]
pub mod testing;
mod window;
pub use descriptor::ExportDescriptor;
pub use failure::SessionFailure;
pub use frame::{CapturedFrame, FramePlacement, FramePublisher};
pub use lifecycle::{
    CaptureBackend, CaptureCounters, CaptureInitializer, CaptureOperation, CaptureSession,
};
pub use session::ProducerSession;
pub use window::ProducerWindows;

#[cfg(test)]
mod tests;
