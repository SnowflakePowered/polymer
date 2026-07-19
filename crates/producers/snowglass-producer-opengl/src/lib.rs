//! OpenGL capture through EXT external memory objects and semaphores.
#![allow(unsafe_code)]

mod interop;
mod swapchain;
mod timeline;
mod tracking;

pub use swapchain::before_swap_buffers;
pub use tracking::Tracking;

pub const BACKEND_NAME: &str = "opengl";

pub(crate) fn log(message: impl std::fmt::Display) {
    use std::io::Write;

    eprintln!("{message}");
    let Some(path) = snowglass_hook::config::opengl_log_file() else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{message}");
    }
}
