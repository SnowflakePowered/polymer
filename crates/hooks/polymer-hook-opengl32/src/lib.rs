//! Early-process OpenGL presentation hooks.
#![allow(unsafe_code)]

mod hooks;
mod install;
mod sys;

#[cfg(test)]
mod tests;

pub use sys::{PolymerInstallOpenGlHook, PolymerUninstallOpenGlHook};
