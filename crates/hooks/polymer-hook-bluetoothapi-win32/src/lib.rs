//! Dolphin native-Windows Wii Remote enumeration and named-pipe redirection.
#![allow(unsafe_code)]

mod configuration;
mod enumeration;
mod handles;
mod hooks;
mod install;
mod runtime;
mod sys;

pub use sys::{PolymerInstallBluetoothApiWin32Hook, PolymerUninstallBluetoothApiWin32Hook};
