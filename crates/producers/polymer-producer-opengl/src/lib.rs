//! OpenGL capture through EXT external memory objects and semaphores.
#![allow(unsafe_code)]

mod interop;
mod swapchain;
mod timeline;
mod tracking;

pub use swapchain::before_swap_buffers;
pub use tracking::Tracking;

pub const BACKEND_NAME: &str = "opengl";
