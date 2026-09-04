//! Early-process Win32 pointer and cursor interception.
#![allow(unsafe_code)]

mod hooks;
mod install;
mod runtime;
mod sys;

pub use sys::{PolymerInstallWin32Hook, PolymerUninstallWin32Hook};
