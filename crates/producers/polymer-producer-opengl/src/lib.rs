//! OpenGL capture through EXT external memory objects and semaphores.
#![allow(unsafe_code)]

mod interop;
mod swapchain;
mod timeline;

pub use swapchain::{after_delete_context, before_delete_context, before_swap_buffers};

pub const BACKEND_NAME: &str = "opengl";
