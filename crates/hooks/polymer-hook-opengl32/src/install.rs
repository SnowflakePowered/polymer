//! Process-wide detour installation and original-procedure routing.

use crate::hooks::Hdc;
use polymer_producer_opengl::Tracking;
use std::{
    ffi::c_void,
    sync::{Arc, OnceLock},
};
use windows_sys::Win32::Foundation::E_FAIL;

pub(crate) type GlDrawArraysFn = unsafe extern "system" fn(u32, i32, i32);
pub(crate) type GlDrawElementsFn = unsafe extern "system" fn(u32, i32, u32, *const c_void);
type GlDrawElementsBaseVertexFn = unsafe extern "system" fn(u32, i32, u32, *const c_void, i32);
type GlDeleteRenderbuffersFn = unsafe extern "system" fn(i32, *const u32);
type GlBlitFramebufferFn =
    unsafe extern "system" fn(i32, i32, i32, i32, i32, i32, i32, i32, u32, u32);
type GlCopyImageSubDataFn = unsafe extern "system" fn(
    u32,
    u32,
    i32,
    i32,
    i32,
    i32,
    u32,
    u32,
    i32,
    i32,
    i32,
    i32,
    i32,
    i32,
    i32,
);

#[link(name = "gdi32")]
unsafe extern "system" {
    #[link_name = "SwapBuffers"]
    pub(crate) fn system_swap_buffers(hdc: Hdc) -> i32;
}

#[link(name = "opengl32")]
unsafe extern "system" {
    #[link_name = "wglDeleteContext"]
    pub(crate) fn system_delete_context(context: *mut c_void) -> i32;
    #[link_name = "wglGetProcAddress"]
    pub(crate) fn system_wgl_get_proc_address(name: *const u8) -> *const c_void;
    #[link_name = "glDeleteTextures"]
    pub(crate) fn system_delete_textures(count: i32, textures: *const u32);
    #[link_name = "wglSwapLayerBuffers"]
    pub(crate) fn system_swap_layer_buffers(hdc: Hdc, planes: u32) -> i32;
    #[link_name = "glDrawArrays"]
    pub(crate) fn system_draw_arrays(mode: u32, first: i32, count: i32);
    #[link_name = "glDrawElements"]
    pub(crate) fn system_draw_elements(
        mode: u32,
        count: i32,
        element_type: u32,
        indices: *const c_void,
    );
}

#[derive(Default)]
pub(crate) struct OpenGlRuntime {
    pub(crate) tracking: Arc<Tracking>,
    pub(crate) draw_arrays: OnceLock<GlDrawArraysFn>,
    pub(crate) draw_elements: OnceLock<GlDrawElementsFn>,
    pub(crate) draw_elements_base_vertex: OnceLock<GlDrawElementsBaseVertexFn>,
    pub(crate) delete_renderbuffers: OnceLock<GlDeleteRenderbuffersFn>,
    pub(crate) delete_renderbuffers_ext: OnceLock<GlDeleteRenderbuffersFn>,
    pub(crate) blit_framebuffer: OnceLock<GlBlitFramebufferFn>,
    pub(crate) copy_image_sub_data: OnceLock<GlCopyImageSubDataFn>,
}

impl OpenGlRuntime {
    pub(crate) fn remember_draw_arrays(&self, address: *const c_void) {
        if address != system_draw_arrays as *const c_void {
            let function = unsafe { std::mem::transmute::<*const c_void, GlDrawArraysFn>(address) };
            let _ = self.draw_arrays.set(function);
        }
    }

    pub(crate) fn remember_draw_elements(&self, address: *const c_void) {
        if address != system_draw_elements as *const c_void {
            let function =
                unsafe { std::mem::transmute::<*const c_void, GlDrawElementsFn>(address) };
            let _ = self.draw_elements.set(function);
        }
    }

    pub(crate) fn remember_draw_elements_base_vertex(&self, address: *const c_void) {
        let function =
            unsafe { std::mem::transmute::<*const c_void, GlDrawElementsBaseVertexFn>(address) };
        let _ = self.draw_elements_base_vertex.set(function);
    }

    pub(crate) fn remember_delete_renderbuffers(&self, address: *const c_void) {
        let function =
            unsafe { std::mem::transmute::<*const c_void, GlDeleteRenderbuffersFn>(address) };
        let _ = self.delete_renderbuffers.set(function);
    }

    pub(crate) fn remember_delete_renderbuffers_ext(&self, address: *const c_void) {
        let function =
            unsafe { std::mem::transmute::<*const c_void, GlDeleteRenderbuffersFn>(address) };
        let _ = self.delete_renderbuffers_ext.set(function);
    }

    pub(crate) fn remember_blit_framebuffer(&self, address: *const c_void) {
        let function =
            unsafe { std::mem::transmute::<*const c_void, GlBlitFramebufferFn>(address) };
        let _ = self.blit_framebuffer.set(function);
    }

    pub(crate) fn remember_copy_image_sub_data(&self, address: *const c_void) {
        let function =
            unsafe { std::mem::transmute::<*const c_void, GlCopyImageSubDataFn>(address) };
        let _ = self.copy_image_sub_data.set(function);
    }
}

pub(crate) unsafe fn unavailable_draw_elements_base_vertex(
    mode: u32,
    count: i32,
    element_type: u32,
    indices: *const c_void,
    base_vertex: i32,
) {
    let _ = (mode, count, element_type, indices, base_vertex);
}

pub(crate) unsafe fn unavailable_blit_framebuffer(
    src_x0: i32,
    src_y0: i32,
    src_x1: i32,
    src_y1: i32,
    dst_x0: i32,
    dst_y0: i32,
    dst_x1: i32,
    dst_y1: i32,
    mask: u32,
    filter: u32,
) {
    let _ = (
        src_x0, src_y0, src_x1, src_y1, dst_x0, dst_y0, dst_x1, dst_y1, mask, filter,
    );
}

#[expect(clippy::too_many_arguments)]
pub(crate) unsafe fn unavailable_copy_image_sub_data(
    src_name: u32,
    src_target: u32,
    src_level: i32,
    src_x: i32,
    src_y: i32,
    src_z: i32,
    dst_name: u32,
    dst_target: u32,
    dst_level: i32,
    dst_x: i32,
    dst_y: i32,
    dst_z: i32,
    width: i32,
    height: i32,
    depth: i32,
) {
    let _ = (
        src_name, src_target, src_level, src_x, src_y, src_z, dst_name, dst_target, dst_level,
        dst_x, dst_y, dst_z, width, height, depth,
    );
}

pub(crate) unsafe fn install() -> i32 {
    crate::hooks::HookInstallation::install().map_or(E_FAIL, |_| 0)
}

pub(crate) fn uninstall() -> i32 {
    crate::hooks::HookInstallation::uninstall().map_or(E_FAIL, |()| 0)
}

pub(crate) unsafe fn unavailable_delete_renderbuffers(_count: i32, _names: *const u32) {}
