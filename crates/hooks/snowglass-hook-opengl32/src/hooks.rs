//! Typed OpenGL hook declarations and capture callbacks.

use crate::hook_chain::{
    __resolve_around_terminal, __resolve_terminal, around_chain, chain, observer_chain,
};
use snowglass_hook::HookChain;
use std::{
    cell::Cell,
    ffi::{CStr, c_void},
};

pub(crate) type Hdc = *mut c_void;

chain! {
    pub(crate) dispatch_swap_buffers, register_swap_buffers;
    observe SwapBuffers;
    chain SWAP_BUFFERS_CHAIN, context SwapBuffersContext;
    hook SwapBuffersHook(hdc: Hdc) -> i32;
    resolve with detour crate::install::DETOURS, swap_buffers -> 0;
}

chain! {
    pub(crate) dispatch_swap_layer_buffers, register_swap_layer_buffers;
    observe SwapLayerBuffers;
    chain SWAP_LAYER_BUFFERS_CHAIN, context SwapLayerBuffersContext;
    hook SwapLayerBuffersHook(hdc: Hdc) -> i32;
    abi (hdc: Hdc, planes: u32);
    resolve with detour crate::install::DETOURS, swap_layer_buffers -> 0;
}

chain! {
    pub(crate) dispatch_draw_arrays, register_draw_arrays;
    observe DrawArrays;
    chain DRAW_ARRAYS_CHAIN, context DrawArraysContext;
    hook DrawArraysHook() -> ();
    abi (mode: u32, first: i32, count: i32);
    resolve with crate::install::call_draw_arrays;
}

chain! {
    pub(crate) dispatch_draw_elements, register_draw_elements;
    observe DrawElements;
    chain DRAW_ELEMENTS_CHAIN, context DrawElementsContext;
    hook DrawElementsHook() -> ();
    abi (mode: u32, count: i32, element_type: u32, indices: *const c_void);
    resolve with crate::install::call_draw_elements;
}

chain! {
    pub(crate) dispatch_draw_elements_base_vertex, register_draw_elements_base_vertex;
    observe DrawElementsBaseVertex;
    chain DRAW_ELEMENTS_BASE_VERTEX_CHAIN, context DrawElementsBaseVertexContext;
    hook DrawElementsBaseVertexHook() -> ();
    abi (mode: u32, count: i32, element_type: u32, indices: *const c_void, base_vertex: i32);
    resolve with crate::install::call_draw_elements_base_vertex;
}

chain! {
    pub(crate) dispatch_blit_framebuffer, register_blit_framebuffer;
    observe BlitFramebuffer;
    chain BLIT_FRAMEBUFFER_CHAIN, context BlitFramebufferContext;
    hook BlitFramebufferHook() -> ();
    abi (
        src_x0: i32, src_y0: i32, src_x1: i32, src_y1: i32,
        dst_x0: i32, dst_y0: i32, dst_x1: i32, dst_y1: i32,
        mask: u32, filter: u32,
    );
    resolve with crate::install::call_blit_framebuffer;
}

chain! {
    pub(crate) dispatch_copy_image_sub_data, register_copy_image_sub_data;
    observe CopyImageSubData;
    chain COPY_IMAGE_SUB_DATA_CHAIN, context CopyImageSubDataContext;
    hook CopyImageSubDataHook(
        src_name: u32, src_target: u32,
        dst_name: u32, dst_target: u32, dst_z: i32,
        width: i32, height: i32,
    ) -> ();
    abi (
        src_name: u32, src_target: u32, src_level: i32, src_x: i32, src_y: i32, src_z: i32,
        dst_name: u32, dst_target: u32, dst_level: i32, dst_x: i32, dst_y: i32, dst_z: i32,
        width: i32, height: i32, depth: i32,
    );
    resolve with crate::install::call_copy_image_sub_data;
}

chain! {
    pub(crate) dispatch_wgl_get_proc_address, register_wgl_get_proc_address;
    around WglGetProcAddress;
    chain WGL_GET_PROC_ADDRESS_CHAIN, context WglGetProcAddressContext;
    hook WglGetProcAddressHook(name: *const u8) -> *const c_void;
    resolve with crate::install::call_wgl_get_proc_address;
}

thread_local! { static CAPTURING: Cell<bool> = const { Cell::new(false) }; }

pub(crate) unsafe fn capture_swap_buffers(hdc: Hdc) {
    CAPTURING.with(|capturing| {
        if capturing.replace(true) {
            return;
        }
        unsafe { snowglass_producer_opengl::before_swap_buffers(hdc as isize) };
        capturing.set(false);
    });
}

pub(crate) unsafe fn track_draw_arrays() {
    unsafe { snowglass_producer_opengl::record_draw_candidate() };
}

pub(crate) unsafe fn track_draw_elements() {
    unsafe { snowglass_producer_opengl::record_draw_candidate() };
}

pub(crate) unsafe fn track_draw_elements_base_vertex() {
    unsafe { snowglass_producer_opengl::record_draw_candidate() };
}

pub(crate) unsafe fn track_blit_framebuffer() {
    // The capture path issues its own blits through the hooked entry points;
    // observing those would feed the external image textures into the graph.
    if CAPTURING.with(|capturing| capturing.get()) {
        return;
    }
    unsafe { snowglass_producer_opengl::record_blit_candidate() };
}

pub(crate) unsafe fn track_copy_image_sub_data(
    src_name: u32,
    src_target: u32,
    dst_name: u32,
    dst_target: u32,
    dst_z: i32,
    width: i32,
    height: i32,
) {
    if CAPTURING.with(|capturing| capturing.get()) {
        return;
    }
    unsafe {
        snowglass_producer_opengl::record_copy_candidate(
            src_name, src_target, dst_name, dst_target, dst_z, width, height,
        )
    };
}

pub(crate) unsafe fn route_proc_address(
    name: *const u8,
    mut context: WglGetProcAddressContext<'_>,
) -> *const c_void {
    let next = *context
        .fp_next()
        .expect("OpenGL hook chains always have a terminal");
    let original = unsafe { next(name, context) };
    if name.is_null() || original.is_null() {
        return original;
    }
    match unsafe { CStr::from_ptr(name.cast()) }.to_bytes() {
        b"glDrawArrays" => {
            crate::install::remember_draw_arrays(original);
            crate::sys::draw_arrays as *const c_void
        }
        b"glDrawElements" => {
            crate::install::remember_draw_elements(original);
            crate::sys::draw_elements as *const c_void
        }
        b"glDrawElementsBaseVertex" => {
            crate::install::remember_draw_elements_base_vertex(original);
            crate::sys::draw_elements_base_vertex as *const c_void
        }
        b"glBlitFramebuffer" => {
            crate::install::remember_blit_framebuffer(original);
            crate::sys::blit_framebuffer as *const c_void
        }
        b"glCopyImageSubData" => {
            crate::install::remember_copy_image_sub_data(original);
            crate::sys::copy_image_sub_data as *const c_void
        }
        _ => original,
    }
}
