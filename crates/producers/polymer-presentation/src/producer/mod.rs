//! Shared producer connection and publication. Native resource creation stays
//! in each graphics backend.
mod descriptor;
mod export;
mod failure;
mod frame;
mod retry;
mod session;
mod window;
pub use descriptor::ExportDescriptor;
pub use failure::SessionFailure;
pub use frame::{CapturedFrame, FramePlacement};
pub use retry::ConnectionRetry;
pub use session::ProducerSession;
pub use window::ProducerWindows;

#[cfg(test)]
mod tests;
