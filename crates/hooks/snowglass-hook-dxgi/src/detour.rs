//! Process-wide detours for DXGI methods whose COM object may predate hook installation.

use retour::GenericDetour;
use std::{ffi::c_void, sync::OnceLock};
use windows::Win32::Graphics::Dxgi::{DXGI_ERROR_INVALID_CALL, IDXGIFactory2_Vtbl};

type CreateSwapchainForHwndFn = unsafe extern "system" fn(
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *const c_void,
    *const c_void,
    *mut c_void,
    *mut *mut c_void,
) -> i32;

static CREATE_SWAPCHAIN_FOR_HWND: OnceLock<GenericDetour<CreateSwapchainForHwndFn>> =
    OnceLock::new();

pub(crate) unsafe fn install_create_swapchain_for_hwnd(factory: *mut c_void) -> bool {
    if let Some(detour) = CREATE_SWAPCHAIN_FOR_HWND.get() {
        return detour.is_enabled();
    }
    if factory.is_null() {
        return false;
    }
    let vtable = unsafe { *factory.cast::<*const IDXGIFactory2_Vtbl>() };
    if vtable.is_null() {
        return false;
    }
    let target = unsafe {
        std::mem::transmute::<usize, CreateSwapchainForHwndFn>(
            (*vtable).CreateSwapChainForHwnd as usize,
        )
    };
    let Ok(detour) =
        (unsafe { GenericDetour::new(target, crate::sys::detoured_create_swapchain_for_hwnd) })
    else {
        return false;
    };
    if CREATE_SWAPCHAIN_FOR_HWND.set(detour).is_err() {
        return CREATE_SWAPCHAIN_FOR_HWND
            .get()
            .is_some_and(GenericDetour::is_enabled);
    }
    unsafe {
        CREATE_SWAPCHAIN_FOR_HWND
            .get()
            .expect("factory detour was just installed")
            .enable()
            .is_ok()
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn call_create_swapchain_for_hwnd(
    this: *mut c_void,
    device: *mut c_void,
    window: *mut c_void,
    desc: *const c_void,
    fullscreen_desc: *const c_void,
    restrict_to_output: *mut c_void,
    swapchain: *mut *mut c_void,
) -> i32 {
    let Some(detour) = CREATE_SWAPCHAIN_FOR_HWND.get() else {
        return DXGI_ERROR_INVALID_CALL.0;
    };
    unsafe {
        detour.call(
            this,
            device,
            window,
            desc,
            fullscreen_desc,
            restrict_to_output,
            swapchain,
        )
    }
}
