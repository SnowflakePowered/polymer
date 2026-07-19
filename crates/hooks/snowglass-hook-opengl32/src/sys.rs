//! ABI trampolines and injector-facing installation entry point.

use crate::hooks::Hdc;
use std::ffi::c_void;

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallOpenGlHook() -> i32 {
    unsafe { crate::install::install() }
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn swap_buffers(hdc: Hdc) -> i32
        => crate::hooks::dispatch_swap_buffers;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn blit_framebuffer(
        src_x0: i32, src_y0: i32, src_x1: i32, src_y1: i32,
        dst_x0: i32, dst_y0: i32, dst_x1: i32, dst_y1: i32,
        mask: u32, filter: u32,
    ) -> () => crate::hooks::dispatch_blit_framebuffer;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn copy_image_sub_data(
        src_name: u32, src_target: u32, src_level: i32, src_x: i32, src_y: i32, src_z: i32,
        dst_name: u32, dst_target: u32, dst_level: i32, dst_x: i32, dst_y: i32, dst_z: i32,
        width: i32, height: i32, depth: i32,
    ) -> () => crate::hooks::dispatch_copy_image_sub_data;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn swap_layer_buffers(hdc: Hdc, planes: u32) -> i32
        => crate::hooks::dispatch_swap_layer_buffers;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn wgl_get_proc_address(name: *const u8) -> *const c_void
        => crate::hooks::dispatch_wgl_get_proc_address;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn draw_arrays(mode: u32, first: i32, count: i32) -> ()
        => crate::hooks::dispatch_draw_arrays;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn draw_elements(
        mode: u32,
        count: i32,
        element_type: u32,
        indices: *const c_void,
    ) -> () => crate::hooks::dispatch_draw_elements;
}

snowglass_hook::trampoline! {
    pub(crate) unsafe extern "system" fn draw_elements_base_vertex(
        mode: u32,
        count: i32,
        element_type: u32,
        indices: *const c_void,
        base_vertex: i32,
    ) -> () => crate::hooks::dispatch_draw_elements_base_vertex;
}
