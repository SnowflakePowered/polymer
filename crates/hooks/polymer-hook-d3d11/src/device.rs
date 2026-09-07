//! D3D11 device discovery and hook installation.

use std::ffi::c_void;
use std::sync::Arc;
use windows::{
    Win32::Graphics::Direct3D11::ID3D11Device,
    core::{IUnknown, Interface},
};

pub(crate) unsafe extern "system" fn claim_swapchain(
    swapchain: *mut c_void,
    creation_device: *mut c_void,
    out: *mut polymer_hook_dxgi::SwapchainOwnerHandle,
) -> bool {
    let Some(unknown) = (unsafe { IUnknown::from_raw_borrowed(&creation_device) }) else {
        return false;
    };
    let Ok(device) = unknown.cast::<ID3D11Device>() else {
        return false;
    };
    let tracking = if let Some(tracking) =
        unsafe { crate::vtable::device_tracking(creation_device) }
    {
        tracking
    } else {
        let Ok(immediate_context) = (unsafe { device.GetImmediateContext() }) else {
            return false;
        };
        let tracking = Arc::new(polymer_producer_d3d11::Tracking::default());
        unsafe {
            crate::vtable::install_immediate_context(immediate_context.as_raw(), tracking.clone())
        };
        tracking
    };
    match unsafe { polymer_producer_d3d11::claim(swapchain, creation_device, tracking) } {
        Some(owner) => {
            unsafe { out.write(owner) };
            true
        }
        None => false,
    }
}

pub(crate) unsafe fn create_device(
    _adapter: *mut c_void,
    _device: *mut *mut c_void,
    _feature_level: *mut i32,
    _immediate_context: *mut *mut c_void,
    result: i32,
) {
    if result >= 0 {
        tracing::debug!("polymer: intercepted D3D11CreateDevice");
    }
    // Do not hook the context yet. D3D12 applications can create auxiliary
    // D3D11 devices for interop, video, or UI work; mutating those contexts
    // destabilizes the otherwise unrelated active backend. The DXGI claim
    // path installs tracking once this device actually owns a D3D11
    // swapchain, before the application can draw into it.
}

pub(crate) unsafe fn create_device_and_swapchain(
    _adapter: *mut c_void,
    swapchain: *mut *mut c_void,
    device: *mut *mut c_void,
    _feature_level: *mut i32,
    _immediate_context: *mut *mut c_void,
    result: i32,
) {
    if result < 0 || swapchain.is_null() {
        return;
    }
    tracing::debug!("polymer: intercepted D3D11CreateDeviceAndSwapChain");
    let swapchain = unsafe { *swapchain };
    if !swapchain.is_null() {
        let creation_device = if device.is_null() {
            std::ptr::null_mut()
        } else {
            unsafe { *device }
        };
        unsafe { polymer_hook_dxgi::register_swapchain(swapchain, creation_device) };
    }
}
