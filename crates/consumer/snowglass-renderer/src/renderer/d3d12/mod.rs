//! D3D12 stream receiving and swapchain composition.

pub mod compositor;
pub mod receiver;

pub use compositor::{D3d12Compositor, DisplayLatencyReport};
pub use receiver::D3d12Receiver;
