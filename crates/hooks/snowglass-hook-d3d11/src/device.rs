//! D3D11 device discovery and hook installation.

use std::ffi::c_void;

pub(crate) unsafe extern "system" fn claim_swapchain(
    swapchain: *mut c_void,
    creation_device: *mut c_void,
    out: *mut snowglass_hook_dxgi::SwapchainOwnerHandle,
) -> bool {
    let tracking = unsafe { crate::vtable::device_tracking(creation_device) }.unwrap_or_default();
    match unsafe { snowglass_producer_d3d11::claim(swapchain, creation_device, tracking) } {
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
    immediate_context: *mut *mut c_void,
    result: i32,
    context: &crate::hooks::CreateDeviceContext,
) {
    if result >= 0 {
        crate::d3d11_log("snowglass: intercepted D3D11CreateDevice");
        unsafe {
            crate::vtable::install_immediate_context(
                immediate_context,
                context.common.tracking.clone(),
            )
        };
    }
}

pub(crate) unsafe fn create_device_and_swapchain(
    _adapter: *mut c_void,
    swapchain: *mut *mut c_void,
    device: *mut *mut c_void,
    _feature_level: *mut i32,
    immediate_context: *mut *mut c_void,
    result: i32,
    context: &crate::hooks::CreateDeviceAndSwapchainContext,
) {
    if result < 0 || swapchain.is_null() {
        return;
    }
    crate::d3d11_log("snowglass: intercepted D3D11CreateDeviceAndSwapChain");
    unsafe {
        crate::vtable::install_immediate_context(immediate_context, context.common.tracking.clone())
    };
    let swapchain = unsafe { *swapchain };
    if !swapchain.is_null() {
        let creation_device = if device.is_null() {
            std::ptr::null_mut()
        } else {
            unsafe { *device }
        };
        unsafe { snowglass_hook_dxgi::register_swapchain(swapchain, creation_device) };
    }
}
