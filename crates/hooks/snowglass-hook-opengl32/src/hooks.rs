//! Typed OpenGL hook declarations and capture callbacks.

use snowglass_hook::HookChain;
use std::{
    cell::Cell,
    ffi::{CStr, c_void},
};

pub(crate) type Hdc = *mut c_void;

snowglass_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(snowglass_platform_windows::InlineDetour<_>)]
    #[installer(state = crate::install::OpenGlRuntime)]
    pub(crate) struct HookInstallation;

    #[trampoline]
    #[terminal(installed(
        target = crate::install::system_swap_buffers,
    ), fallback = 0)]
    #[prefix(capture_swap_buffers(hdc))]
    pub(crate) unsafe extern "system" fn swap_buffers(hdc: Hdc) -> i32;

    #[trampoline]
    #[terminal(installed(
        target = crate::install::system_swap_layer_buffers,
    ), fallback = 0)]
    #[prefix(capture_swap_layer_buffers(hdc))]
    pub(crate) unsafe extern "system" fn swap_layer_buffers(hdc: Hdc, planes: u32) -> i32;

    #[trampoline]
    #[terminal(installed(
        target = crate::install::system_draw_arrays,
    ), fallback = ())]
    #[prefix(track_draw_arrays(first, count))]
    #[infix(route_draw_arrays)]
    pub(crate) unsafe extern "system" fn draw_arrays(mode: u32, first: i32, count: i32);

    #[trampoline]
    #[terminal(installed(
        target = crate::install::system_draw_elements,
    ), fallback = ())]
    #[prefix(track_draw_elements())]
    #[infix(route_draw_elements)]
    pub(crate) unsafe extern "system" fn draw_elements(
        mode: u32,
        count: i32,
        element_type: u32,
        indices: *const c_void,
    );

    #[trampoline]
    #[terminal(direct = crate::install::unavailable_draw_elements_base_vertex)]
    #[prefix(track_draw_elements_base_vertex())]
    #[infix(route_draw_elements_base_vertex)]
    pub(crate) unsafe extern "system" fn draw_elements_base_vertex(
        mode: u32,
        count: i32,
        element_type: u32,
        indices: *const c_void,
        base_vertex: i32,
    );

    #[trampoline]
    #[terminal(direct = crate::install::unavailable_blit_framebuffer)]
    #[prefix(track_blit_framebuffer())]
    #[infix(route_blit_framebuffer)]
    pub(crate) unsafe extern "system" fn blit_framebuffer(
        src_x0: i32, src_y0: i32, src_x1: i32, src_y1: i32,
        dst_x0: i32, dst_y0: i32, dst_x1: i32, dst_y1: i32,
        mask: u32, filter: u32,
    );

    #[trampoline]
    #[terminal(direct = crate::install::unavailable_copy_image_sub_data)]
    #[prefix(track_copy_image_sub_data(
        src_name, src_target, dst_name, dst_target, dst_z, width, height
    ))]
    #[infix(route_copy_image_sub_data)]
    pub(crate) unsafe extern "system" fn copy_image_sub_data(
        src_name: u32, src_target: u32, src_level: i32, src_x: i32, src_y: i32, src_z: i32,
        dst_name: u32, dst_target: u32, dst_level: i32, dst_x: i32, dst_y: i32, dst_z: i32,
        width: i32, height: i32, depth: i32,
    );

    #[trampoline]
    #[terminal(installed(
        target = crate::install::system_wgl_get_proc_address,
    ), fallback = std::ptr::null())]
    #[infix(route_proc_address)]
    pub(crate) unsafe extern "system" fn wgl_get_proc_address(
        name: *const u8,
    ) -> *const c_void;
}

thread_local! { static CAPTURING: Cell<bool> = const { Cell::new(false) }; }

pub(crate) unsafe fn capture_swap_buffers(hdc: Hdc, context: &SwapBuffersContext) {
    CAPTURING.with(|capturing| {
        if capturing.replace(true) {
            return;
        }
        unsafe {
            snowglass_producer_opengl::before_swap_buffers(
                &context.state.tracking,
                hdc as isize,
            )
        };
        capturing.set(false);
    });
}

pub(crate) unsafe fn capture_swap_layer_buffers(hdc: Hdc, context: &SwapLayerBuffersContext) {
    CAPTURING.with(|capturing| {
        if capturing.replace(true) {
            return;
        }
        unsafe {
            snowglass_producer_opengl::before_swap_buffers(
                &context.state.tracking,
                hdc as isize,
            )
        };
        capturing.set(false);
    });
}

pub(crate) unsafe fn track_draw_arrays(first: i32, count: i32, context: &DrawArraysContext) {
    unsafe {
        context
            .state
            .tracking
            .record_draw_candidate(Some((first, count)))
    };
}

pub(crate) unsafe fn track_draw_elements(context: &DrawElementsContext) {
    unsafe { context.state.tracking.record_draw_candidate(None) };
}

pub(crate) unsafe fn route_draw_arrays(
    mode: u32,
    first: i32,
    count: i32,
    mut next: DrawArraysContext<'_>,
) {
    if let Some(original) = next.state.draw_arrays.get() {
        unsafe { original(mode, first, count) };
    } else {
        snowglass_hook::next!(next, mode, first, count);
    }
}

pub(crate) unsafe fn route_draw_elements(
    mode: u32,
    count: i32,
    element_type: u32,
    indices: *const c_void,
    mut next: DrawElementsContext<'_>,
) {
    if let Some(original) = next.state.draw_elements.get() {
        unsafe { original(mode, count, element_type, indices) };
    } else {
        snowglass_hook::next!(next, mode, count, element_type, indices);
    }
}

pub(crate) unsafe fn track_draw_elements_base_vertex(context: &DrawElementsBaseVertexContext) {
    unsafe { context.state.tracking.record_draw_candidate(None) };
}

pub(crate) unsafe fn route_draw_elements_base_vertex(
    mode: u32,
    count: i32,
    element_type: u32,
    indices: *const c_void,
    base_vertex: i32,
    next: DrawElementsBaseVertexContext<'_>,
) {
    if let Some(original) = next.state.draw_elements_base_vertex.get() {
        unsafe { original(mode, count, element_type, indices, base_vertex) };
    }
}

pub(crate) unsafe fn track_blit_framebuffer(context: &BlitFramebufferContext) {
    // The capture path issues its own blits through the hooked entry points;
    // observing those would feed the external image textures into the graph.
    if CAPTURING.with(|capturing| capturing.get()) {
        return;
    }
    unsafe { context.state.tracking.record_blit_candidate() };
}

#[expect(clippy::too_many_arguments)]
pub(crate) unsafe fn route_blit_framebuffer(
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
    next: BlitFramebufferContext<'_>,
) {
    if let Some(original) = next.state.blit_framebuffer.get() {
        unsafe {
            original(
                src_x0, src_y0, src_x1, src_y1, dst_x0, dst_y0, dst_x1, dst_y1, mask, filter,
            )
        };
    }
}

pub(crate) unsafe fn track_copy_image_sub_data(
    src_name: u32,
    src_target: u32,
    dst_name: u32,
    dst_target: u32,
    dst_z: i32,
    width: i32,
    height: i32,
    context: &CopyImageSubDataContext,
) {
    if CAPTURING.with(|capturing| capturing.get()) {
        return;
    }
    unsafe {
        context.state.tracking.record_copy_candidate(
            src_name, src_target, dst_name, dst_target, dst_z, width, height,
        )
    };
}

#[expect(clippy::too_many_arguments)]
pub(crate) unsafe fn route_copy_image_sub_data(
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
    next: CopyImageSubDataContext<'_>,
) {
    if let Some(original) = next.state.copy_image_sub_data.get() {
        unsafe {
            original(
                src_name, src_target, src_level, src_x, src_y, src_z, dst_name, dst_target,
                dst_level, dst_x, dst_y, dst_z, width, height, depth,
            )
        };
    }
}

pub(crate) unsafe fn route_proc_address(
    name: *const u8,
    mut context: WglGetProcAddressContext<'_>,
) -> *const c_void {
    let next = *context
        .fp_next()
        .expect("OpenGL hook chains always have a terminal");
    let state = context.state.clone();
    let original = unsafe { next(name, context) };
    if name.is_null() || original.is_null() {
        return original;
    }
    match unsafe { CStr::from_ptr(name.cast()) }.to_bytes() {
        b"glDrawArrays" => {
            state.remember_draw_arrays(original);
            crate::hooks::draw_arrays as *const c_void
        }
        b"glDrawElements" => {
            state.remember_draw_elements(original);
            crate::hooks::draw_elements as *const c_void
        }
        b"glDrawElementsBaseVertex" => {
            state.remember_draw_elements_base_vertex(original);
            crate::hooks::draw_elements_base_vertex as *const c_void
        }
        b"glBlitFramebuffer" => {
            state.remember_blit_framebuffer(original);
            crate::hooks::blit_framebuffer as *const c_void
        }
        b"glCopyImageSubData" => {
            state.remember_copy_image_sub_data(original);
            crate::hooks::copy_image_sub_data as *const c_void
        }
        _ => original,
    }
}
