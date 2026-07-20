//! SDL3 virtual gamepad injection.
#![allow(unsafe_code)]

mod api;
mod install;

pub use install::SnowglassInstallSdl3Hook;
