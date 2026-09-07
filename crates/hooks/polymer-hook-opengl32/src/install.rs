//! Presentation and context-lifetime detours.
use crate::hooks::Hdc;
use std::ffi::c_void;
use windows_sys::Win32::Foundation::E_FAIL;
#[link(name = "user32")]
unsafe extern "system" {
    #[link_name = "GetClientRect"]
    pub(crate) fn system_get_client_rect(
        window: *mut c_void,
        rect: *mut windows_sys::Win32::Foundation::RECT,
    ) -> i32;
}
#[link(name = "gdi32")]
unsafe extern "system" {
    #[link_name = "SwapBuffers"]
    pub(crate) fn system_swap_buffers(hdc: Hdc) -> i32;
}

#[link(name = "opengl32")]
unsafe extern "system" {
    #[link_name = "wglMakeCurrent"]
    pub(crate) fn system_make_current(dc: Hdc, context: *mut c_void) -> i32;
    #[link_name = "wglGetCurrentDC"]
    pub(crate) fn system_get_current_dc() -> Hdc;
    #[link_name = "wglGetProcAddress"]
    pub(crate) fn system_get_proc_address(name: *const u8) -> *const c_void;
    #[link_name = "wglDeleteContext"]
    pub(crate) fn system_delete_context(context: *mut c_void) -> i32;
    #[link_name = "wglSwapLayerBuffers"]
    pub(crate) fn system_swap_layer_buffers(hdc: Hdc, planes: u32) -> i32;
}

pub(crate) unsafe fn install() -> i32 {
    if polymer_hook::config::require_swapchain().is_err() {
        return windows_sys::Win32::Foundation::E_INVALIDARG;
    }
    crate::hooks::HookInstallation::install().map_or(E_FAIL, |_| 0)
}
pub(crate) fn uninstall() -> i32 {
    crate::hooks::HookInstallation::uninstall().map_or(E_FAIL, |()| 0)
}
