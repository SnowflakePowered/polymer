//! Early-process OpenGL presentation hooks.
#![allow(unsafe_code)]

mod hooks;
mod install;
mod sys;

pub use sys::{PolymerInstallOpenGlHook, PolymerUninstallOpenGlHook};
