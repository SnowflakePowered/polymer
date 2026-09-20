//! SDL3 virtual gamepad injection.
#![allow(unsafe_code)]

mod api;
mod install;

#[cfg(target_os = "windows")]
pub use install::PolymerInstallSdl3Hook;
#[cfg(target_os = "macos")]
pub use install::SDL_DYNAPI_entry;
