//! Early-process GDI paint redirection hooks.
#![allow(unsafe_code)]

mod hooks;
mod install;
mod runtime;
mod sys;

pub use sys::PolymerInstallGdiHook;
