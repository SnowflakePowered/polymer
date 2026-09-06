//! OpenGL capture through EXT external memory objects and semaphores.
#![allow(unsafe_code)]

#[cfg(test)]
mod application;
mod interop;
mod swapchain;
mod timeline;
mod tracking;

pub use swapchain::{after_delete_context, before_delete_context, before_swap_buffers};
pub use tracking::Tracking;

pub const BACKEND_NAME: &str = "opengl";
