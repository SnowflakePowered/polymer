//! Presentation and context-lifetime detours.
use crate::hooks::Hdc;
use std::ffi::c_void;
use windows_sys::Win32::Foundation::E_FAIL;
#[link(name = "gdi32")]
unsafe extern "system" {
    #[link_name = "SwapBuffers"]
    pub(crate) fn system_swap_buffers(hdc: Hdc) -> i32;
}

#[link(name = "opengl32")]
unsafe extern "system" {
    #[link_name = "wglDeleteContext"]
    pub(crate) fn system_delete_context(context: *mut c_void) -> i32;
    #[link_name = "wglSwapLayerBuffers"]
    pub(crate) fn system_swap_layer_buffers(hdc: Hdc, planes: u32) -> i32;
}

pub(crate) unsafe fn install() -> i32 {
    crate::hooks::HookInstallation::install().map_or(E_FAIL, |_| 0)
}
pub(crate) fn uninstall() -> i32 {
    crate::hooks::HookInstallation::uninstall().map_or(E_FAIL, |()| 0)
}
