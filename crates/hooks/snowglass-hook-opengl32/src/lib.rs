//! Early-process OpenGL presentation hooks.
#![allow(unsafe_code)]

mod hook_chain;
mod hooks;
mod install;
mod sys;

pub use sys::SnowglassInstallOpenGlHook;
