//! Early-process OpenGL presentation hooks.
#![allow(unsafe_code)]

use retour::GenericDetour;
use std::{
    cell::Cell,
    ffi::{CStr, c_void},
    sync::OnceLock,
};
use windows_sys::Win32::Foundation::E_FAIL;

type Hdc = *mut c_void;
type SwapBuffersFn = unsafe extern "system" fn(Hdc) -> i32;
type SwapLayerBuffersFn = unsafe extern "system" fn(Hdc, u32) -> i32;
type WglGetProcAddressFn = unsafe extern "system" fn(*const u8) -> *const c_void;
type GlDrawArraysFn = unsafe extern "system" fn(u32, i32, i32);
type GlDrawElementsFn = unsafe extern "system" fn(u32, i32, u32, *const c_void);
type GlDrawElementsBaseVertexFn = unsafe extern "system" fn(u32, i32, u32, *const c_void, i32);

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

static SWAP_BUFFERS: OnceLock<GenericDetour<SwapBuffersFn>> = OnceLock::new();
static SWAP_LAYER_BUFFERS: OnceLock<GenericDetour<SwapLayerBuffersFn>> = OnceLock::new();
static WGL_GET_PROC_ADDRESS: OnceLock<GenericDetour<WglGetProcAddressFn>> = OnceLock::new();
static CORE_DRAW_ARRAYS: OnceLock<GenericDetour<GlDrawArraysFn>> = OnceLock::new();
static CORE_DRAW_ELEMENTS: OnceLock<GenericDetour<GlDrawElementsFn>> = OnceLock::new();
static DRAW_ARRAYS: OnceLock<GlDrawArraysFn> = OnceLock::new();
static DRAW_ELEMENTS: OnceLock<GlDrawElementsFn> = OnceLock::new();
static DRAW_ELEMENTS_BASE_VERTEX: OnceLock<GlDrawElementsBaseVertexFn> = OnceLock::new();

thread_local! { static CAPTURING: Cell<bool> = const { Cell::new(false) }; }

unsafe fn capture(hdc: Hdc) {
    CAPTURING.with(|capturing| {
        if capturing.replace(true) {
            return;
        }
        unsafe { snowglass_producer_opengl::before_swap_buffers(hdc as isize) };
        capturing.set(false);
    });
}

unsafe extern "system" fn swap_buffers(hdc: Hdc) -> i32 {
    // Capture while the application's back buffer is still the presented image.
    unsafe { capture(hdc) };
    let Some(detour) = SWAP_BUFFERS.get() else {
        return 0;
    };
    unsafe { detour.call(hdc) }
}

unsafe extern "system" fn swap_layer_buffers(hdc: Hdc, planes: u32) -> i32 {
    unsafe { capture(hdc) };
    let Some(detour) = SWAP_LAYER_BUFFERS.get() else {
        return 0;
    };
    unsafe { detour.call(hdc, planes) }
}

unsafe extern "system" fn draw_arrays(mode: u32, first: i32, count: i32) {
    unsafe { snowglass_producer_opengl::record_draw_candidate() };
    if let Some(original) = DRAW_ARRAYS.get() {
        unsafe { original(mode, first, count) };
    } else if let Some(detour) = CORE_DRAW_ARRAYS.get() {
        unsafe { detour.call(mode, first, count) };
    }
}

unsafe extern "system" fn draw_elements(
    mode: u32,
    count: i32,
    element_type: u32,
    indices: *const c_void,
) {
    unsafe { snowglass_producer_opengl::record_draw_candidate() };
    if let Some(original) = DRAW_ELEMENTS.get() {
        unsafe { original(mode, count, element_type, indices) };
    } else if let Some(detour) = CORE_DRAW_ELEMENTS.get() {
        unsafe { detour.call(mode, count, element_type, indices) };
    }
}

unsafe extern "system" fn draw_elements_base_vertex(
    mode: u32,
    count: i32,
    element_type: u32,
    indices: *const c_void,
    base_vertex: i32,
) {
    unsafe { snowglass_producer_opengl::record_draw_candidate() };
    if let Some(original) = DRAW_ELEMENTS_BASE_VERTEX.get() {
        unsafe { original(mode, count, element_type, indices, base_vertex) };
    }
}

unsafe extern "system" fn wgl_get_proc_address(name: *const u8) -> *const c_void {
    let Some(detour) = WGL_GET_PROC_ADDRESS.get() else {
        return std::ptr::null();
    };
    let original = unsafe { detour.call(name) };
    if name.is_null() || original.is_null() {
        return original;
    }
    let name = unsafe { CStr::from_ptr(name.cast()) }.to_bytes();
    match name {
        b"glDrawArrays" => {
            let function: GlDrawArraysFn = unsafe { std::mem::transmute(original) };
            if original != system_draw_arrays as *const c_void {
                let _ = DRAW_ARRAYS.set(function);
            }
            draw_arrays as *const c_void
        }
        b"glDrawElements" => {
            let function: GlDrawElementsFn = unsafe { std::mem::transmute(original) };
            if original != system_draw_elements as *const c_void {
                let _ = DRAW_ELEMENTS.set(function);
            }
            draw_elements as *const c_void
        }
        b"glDrawElementsBaseVertex" => {
            let function: GlDrawElementsBaseVertexFn = unsafe { std::mem::transmute(original) };
            let _ = DRAW_ELEMENTS_BASE_VERTEX.set(function);
            draw_elements_base_vertex as *const c_void
        }
        _ => original,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn SnowglassInstallOpenGlHook() -> i32 {
    if let Some(detour) = SWAP_BUFFERS.get() {
        return if detour.is_enabled()
            && SWAP_LAYER_BUFFERS
                .get()
                .is_some_and(|hook| hook.is_enabled())
            && WGL_GET_PROC_ADDRESS
                .get()
                .is_some_and(|hook| hook.is_enabled())
            && CORE_DRAW_ARRAYS.get().is_some_and(|hook| hook.is_enabled())
            && CORE_DRAW_ELEMENTS
                .get()
                .is_some_and(|hook| hook.is_enabled())
        {
            0
        } else {
            E_FAIL
        };
    }
    let detour = match unsafe {
        GenericDetour::new(
            system_swap_buffers as SwapBuffersFn,
            swap_buffers as SwapBuffersFn,
        )
    } {
        Ok(detour) => detour,
        Err(_) => return E_FAIL,
    };
    if SWAP_BUFFERS.set(detour).is_err() {
        return E_FAIL;
    }
    let layer_detour = match unsafe {
        GenericDetour::new(
            system_swap_layer_buffers as SwapLayerBuffersFn,
            swap_layer_buffers as SwapLayerBuffersFn,
        )
    } {
        Ok(detour) => detour,
        Err(_) => return E_FAIL,
    };
    if SWAP_LAYER_BUFFERS.set(layer_detour).is_err() {
        return E_FAIL;
    }
    let proc_detour = match unsafe {
        GenericDetour::new(
            system_wgl_get_proc_address as WglGetProcAddressFn,
            wgl_get_proc_address as WglGetProcAddressFn,
        )
    } {
        Ok(detour) => detour,
        Err(_) => return E_FAIL,
    };
    if WGL_GET_PROC_ADDRESS.set(proc_detour).is_err() {
        return E_FAIL;
    }
    let arrays_detour = match unsafe {
        GenericDetour::new(
            system_draw_arrays as GlDrawArraysFn,
            draw_arrays as GlDrawArraysFn,
        )
    } {
        Ok(detour) => detour,
        Err(_) => return E_FAIL,
    };
    if CORE_DRAW_ARRAYS.set(arrays_detour).is_err() {
        return E_FAIL;
    }
    let elements_detour = match unsafe {
        GenericDetour::new(
            system_draw_elements as GlDrawElementsFn,
            draw_elements as GlDrawElementsFn,
        )
    } {
        Ok(detour) => detour,
        Err(_) => return E_FAIL,
    };
    if CORE_DRAW_ELEMENTS.set(elements_detour).is_err() {
        return E_FAIL;
    }
    let enabled = unsafe {
        WGL_GET_PROC_ADDRESS
            .get()
            .expect("proc detour initialized")
            .enable()
            .and_then(|()| {
                CORE_DRAW_ARRAYS
                    .get()
                    .expect("arrays detour initialized")
                    .enable()
            })
            .and_then(|()| {
                CORE_DRAW_ELEMENTS
                    .get()
                    .expect("elements detour initialized")
                    .enable()
            })
            .and_then(|()| {
                SWAP_LAYER_BUFFERS
                    .get()
                    .expect("layer detour initialized")
                    .enable()
            })
            .and_then(|()| {
                SWAP_BUFFERS
                    .get()
                    .expect("swap detour initialized")
                    .enable()
            })
    };
    match enabled {
        Ok(()) => 0,
        Err(_) => {
            let _ = unsafe {
                CORE_DRAW_ELEMENTS
                    .get()
                    .expect("elements detour initialized")
                    .disable()
            };
            let _ = unsafe {
                CORE_DRAW_ARRAYS
                    .get()
                    .expect("arrays detour initialized")
                    .disable()
            };
            let _ = unsafe {
                SWAP_LAYER_BUFFERS
                    .get()
                    .expect("layer detour initialized")
                    .disable()
            };
            let _ = unsafe {
                WGL_GET_PROC_ADDRESS
                    .get()
                    .expect("proc detour initialized")
                    .disable()
            };
            E_FAIL
        }
    }
}
