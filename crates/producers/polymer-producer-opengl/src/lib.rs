//! Fixed WGL pbuffer swapchains, exported through EXT memory objects and semaphores.
#![allow(unsafe_code)]

mod interop;
mod swapchain;
mod timeline;

pub use swapchain::wgl::WglSwapchain;
pub use swapchain::{after_delete_context, before_delete_context};

pub const BACKEND_NAME: &str = "opengl";
