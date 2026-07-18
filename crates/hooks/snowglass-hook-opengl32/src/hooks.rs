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
        _ => original,
    }
}
