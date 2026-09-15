//! Early-process DirectInput detour and COM device interception.
#![allow(unsafe_code)]

mod hooks;
mod install;
mod runtime;
mod sys;
mod vtable;

pub use sys::{PolymerInstallDinputHook, PolymerUninstallDinputHook};
