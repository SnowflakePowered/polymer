//! D3D11 export detours and COM device bootstrap.
#![allow(unsafe_code)]

mod ddi;
mod device;
mod hooks;
mod install;
mod sys;
mod virtual_creation;
mod vtable;

pub use sys::{PolymerInstallD3d11Hook, PolymerUninstallD3d11Hook};
