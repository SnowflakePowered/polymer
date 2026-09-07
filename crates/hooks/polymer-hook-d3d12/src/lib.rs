//! Register native D3D12 swapchain creation with the central DXGI hook.
#![allow(unsafe_code)]

mod sys;
pub use sys::{PolymerInstallD3d12Hook, PolymerUninstallD3d12Hook};
