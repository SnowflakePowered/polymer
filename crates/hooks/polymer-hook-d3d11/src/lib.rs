//! D3D11 export detours and COM device bootstrap.
#![allow(unsafe_code)]

mod device;
mod hooks;
mod install;
mod sys;
mod vtable;

pub use sys::{PolymerInstallD3d11Hook, PolymerUninstallD3d11Hook};

#[cfg(test)]
mod tests;
