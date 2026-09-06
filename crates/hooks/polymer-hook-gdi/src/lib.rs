//! Early-process GDI paint redirection hooks.
#![allow(unsafe_code)]

mod hooks;
mod install;
mod runtime;
mod sys;

#[cfg(test)]
mod tests;

pub use sys::PolymerInstallGdiHook;
