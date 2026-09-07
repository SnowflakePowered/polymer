//! D3D12 export detours, DXGI ownership, and swapchain capture.
#![allow(unsafe_code)]

mod hooks;
mod install;
mod sys;
mod vtable;

pub use sys::{PolymerInstallD3d12Hook, PolymerUninstallD3d12Hook};

unsafe extern "system" fn claim_swapchain(
    swapchain: *mut std::ffi::c_void,
    queue: *mut std::ffi::c_void,
    out: *mut polymer_hook_dxgi::SwapchainOwnerHandle,
) -> bool {
    let tracking = unsafe { vtable::queue_tracking(queue) }.unwrap_or_default();
    match unsafe { polymer_producer_d3d12::claim(swapchain, queue, tracking) } {
        Some(owner) => {
            unsafe { out.write(owner) };
            true
        }
        None => false,
    }
}
