//! Process-wide detour installation and original-procedure routing.

use crate::hooks::Hdc;
use retour::GenericDetour;
use snowglass_producer_opengl::Tracking;
use std::{
    ffi::c_void,
    sync::{Arc, OnceLock},
};
use windows_sys::Win32::Foundation::E_FAIL;

type SwapBuffersFn = unsafe extern "system" fn(Hdc) -> i32;
type SwapLayerBuffersFn = unsafe extern "system" fn(Hdc, u32) -> i32;
type WglGetProcAddressFn = unsafe extern "system" fn(*const u8) -> *const c_void;
type GlDrawArraysFn = unsafe extern "system" fn(u32, i32, i32);
type GlDrawElementsFn = unsafe extern "system" fn(u32, i32, u32, *const c_void);
type GlDrawElementsBaseVertexFn = unsafe extern "system" fn(u32, i32, u32, *const c_void, i32);
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
    fn system_swap_buffers(hdc: Hdc) -> i32;
}

#[link(name = "opengl32")]
unsafe extern "system" {
    #[link_name = "wglGetProcAddress"]
    fn system_wgl_get_proc_address(name: *const u8) -> *const c_void;
    #[link_name = "wglSwapLayerBuffers"]
    fn system_swap_layer_buffers(hdc: Hdc, planes: u32) -> i32;
    #[link_name = "glDrawArrays"]
    fn system_draw_arrays(mode: u32, first: i32, count: i32);
    #[link_name = "glDrawElements"]
    fn system_draw_elements(mode: u32, count: i32, element_type: u32, indices: *const c_void);
}

pub(crate) struct OpenGlDetours {
    pub(crate) swap_buffers: GenericDetour<SwapBuffersFn>,
    pub(crate) swap_layer_buffers: GenericDetour<SwapLayerBuffersFn>,
    pub(crate) wgl_get_proc_address: GenericDetour<WglGetProcAddressFn>,
    core_draw_arrays: GenericDetour<GlDrawArraysFn>,
    core_draw_elements: GenericDetour<GlDrawElementsFn>,
    tracking: Arc<Tracking>,
}

pub(crate) static DETOURS: OnceLock<OpenGlDetours> = OnceLock::new();
static DRAW_ARRAYS: OnceLock<GlDrawArraysFn> = OnceLock::new();
static DRAW_ELEMENTS: OnceLock<GlDrawElementsFn> = OnceLock::new();
static DRAW_ELEMENTS_BASE_VERTEX: OnceLock<GlDrawElementsBaseVertexFn> = OnceLock::new();
static BLIT_FRAMEBUFFER: OnceLock<GlBlitFramebufferFn> = OnceLock::new();
static COPY_IMAGE_SUB_DATA: OnceLock<GlCopyImageSubDataFn> = OnceLock::new();

pub(crate) fn tracking() -> Arc<Tracking> {
    DETOURS
        .get()
        .map(|detours| detours.tracking.clone())
        .unwrap_or_default()
}

pub(crate) fn remember_draw_arrays(address: *const c_void) {
    if address != system_draw_arrays as *const c_void {
        let function = unsafe { std::mem::transmute::<*const c_void, GlDrawArraysFn>(address) };
        let _ = DRAW_ARRAYS.set(function);
    }
}

pub(crate) fn remember_draw_elements(address: *const c_void) {
    if address != system_draw_elements as *const c_void {
        let function = unsafe { std::mem::transmute::<*const c_void, GlDrawElementsFn>(address) };
        let _ = DRAW_ELEMENTS.set(function);
    }
}

pub(crate) fn remember_draw_elements_base_vertex(address: *const c_void) {
    let function =
        unsafe { std::mem::transmute::<*const c_void, GlDrawElementsBaseVertexFn>(address) };
    let _ = DRAW_ELEMENTS_BASE_VERTEX.set(function);
}

pub(crate) fn remember_blit_framebuffer(address: *const c_void) {
    let function = unsafe { std::mem::transmute::<*const c_void, GlBlitFramebufferFn>(address) };
    let _ = BLIT_FRAMEBUFFER.set(function);
}

pub(crate) fn remember_copy_image_sub_data(address: *const c_void) {
    let function = unsafe { std::mem::transmute::<*const c_void, GlCopyImageSubDataFn>(address) };
    let _ = COPY_IMAGE_SUB_DATA.set(function);
}

pub(crate) unsafe fn call_draw_arrays(mode: u32, first: i32, count: i32) {
    if let Some(original) = DRAW_ARRAYS.get() {
        unsafe { original(mode, first, count) };
    } else if let Some(detours) = DETOURS.get() {
        unsafe { detours.core_draw_arrays.call(mode, first, count) };
    }
}

pub(crate) unsafe fn call_draw_elements(
    mode: u32,
    count: i32,
    element_type: u32,
    indices: *const c_void,
) {
    if let Some(original) = DRAW_ELEMENTS.get() {
        unsafe { original(mode, count, element_type, indices) };
    } else if let Some(detours) = DETOURS.get() {
        unsafe {
            detours
                .core_draw_elements
                .call(mode, count, element_type, indices)
        };
    }
}

pub(crate) unsafe fn call_draw_elements_base_vertex(
    mode: u32,
    count: i32,
    element_type: u32,
    indices: *const c_void,
    base_vertex: i32,
) {
    if let Some(original) = DRAW_ELEMENTS_BASE_VERTEX.get() {
        unsafe { original(mode, count, element_type, indices, base_vertex) };
    }
}

pub(crate) unsafe fn call_blit_framebuffer(
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
    if let Some(original) = BLIT_FRAMEBUFFER.get() {
        unsafe {
            original(
                src_x0, src_y0, src_x1, src_y1, dst_x0, dst_y0, dst_x1, dst_y1, mask, filter,
            )
        };
    }
}

#[expect(clippy::too_many_arguments)]
pub(crate) unsafe fn call_copy_image_sub_data(
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
    if let Some(original) = COPY_IMAGE_SUB_DATA.get() {
        unsafe {
            original(
                src_name, src_target, src_level, src_x, src_y, src_z, dst_name, dst_target,
                dst_level, dst_x, dst_y, dst_z, width, height, depth,
            )
        };
    }
}

pub(crate) unsafe fn call_wgl_get_proc_address(
    name: *const u8,
    _context: crate::hooks::WglGetProcAddressContext<'_>,
) -> *const c_void {
    DETOURS.get().map_or(std::ptr::null(), |detours| unsafe {
        detours.wgl_get_proc_address.call(name)
    })
}

pub(crate) unsafe fn install() -> i32 {
    if let Some(detours) = DETOURS.get() {
        return if all_enabled(detours) { 0 } else { E_FAIL };
    }
    let swap_buffers = match unsafe {
        GenericDetour::new(
            system_swap_buffers as SwapBuffersFn,
            crate::sys::swap_buffers,
        )
    } {
        Ok(detour) => detour,
        Err(_) => return E_FAIL,
    };
    let swap_layer_buffers = match unsafe {
        GenericDetour::new(
            system_swap_layer_buffers as SwapLayerBuffersFn,
            crate::sys::swap_layer_buffers,
        )
    } {
        Ok(detour) => detour,
        Err(_) => return E_FAIL,
    };
    let wgl_get_proc_address = match unsafe {
        GenericDetour::new(
            system_wgl_get_proc_address as WglGetProcAddressFn,
            crate::sys::wgl_get_proc_address,
        )
    } {
        Ok(detour) => detour,
        Err(_) => return E_FAIL,
    };
    let core_draw_arrays = match unsafe {
        GenericDetour::new(
            system_draw_arrays as GlDrawArraysFn,
            crate::sys::draw_arrays,
        )
    } {
        Ok(detour) => detour,
        Err(_) => return E_FAIL,
    };
    let core_draw_elements = match unsafe {
        GenericDetour::new(
            system_draw_elements as GlDrawElementsFn,
            crate::sys::draw_elements,
        )
    } {
        Ok(detour) => detour,
        Err(_) => return E_FAIL,
    };
    if DETOURS
        .set(OpenGlDetours {
            swap_buffers,
            swap_layer_buffers,
            wgl_get_proc_address,
            core_draw_arrays,
            core_draw_elements,
            tracking: Arc::default(),
        })
        .is_err()
    {
        return E_FAIL;
    }
    let detours = DETOURS.get().expect("detours were just initialized");
    let enabled = unsafe {
        detours
            .wgl_get_proc_address
            .enable()
            .and_then(|()| detours.core_draw_arrays.enable())
            .and_then(|()| detours.core_draw_elements.enable())
            .and_then(|()| detours.swap_layer_buffers.enable())
            .and_then(|()| detours.swap_buffers.enable())
    };
    if enabled.is_err() {
        unsafe { disable_all(detours) };
        return E_FAIL;
    }
    0
}

fn all_enabled(detours: &OpenGlDetours) -> bool {
    detours.swap_buffers.is_enabled()
        && detours.swap_layer_buffers.is_enabled()
        && detours.wgl_get_proc_address.is_enabled()
        && detours.core_draw_arrays.is_enabled()
        && detours.core_draw_elements.is_enabled()
}

unsafe fn disable_all(detours: &OpenGlDetours) {
    let _ = unsafe { detours.swap_buffers.disable() };
    let _ = unsafe { detours.swap_layer_buffers.disable() };
    let _ = unsafe { detours.core_draw_elements.disable() };
    let _ = unsafe { detours.core_draw_arrays.disable() };
    let _ = unsafe { detours.wgl_get_proc_address.disable() };
}
