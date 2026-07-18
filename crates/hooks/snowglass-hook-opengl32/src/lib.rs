//! Early-process OpenGL presentation hooks.
#![allow(unsafe_code)]

mod hook_chain;
mod hooks;
mod install;
mod sys;

pub use sys::SnowglassInstallOpenGlHook;

pub(crate) fn register_hooks() {
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| {
        hooks::register_swap_buffers(hooks::capture_swap_buffers);
        hooks::register_swap_layer_buffers(hooks::capture_swap_buffers);
        hooks::register_draw_arrays(hooks::track_draw_arrays);
        hooks::register_draw_elements(hooks::track_draw_elements);
        hooks::register_draw_elements_base_vertex(hooks::track_draw_elements_base_vertex);
        hooks::register_wgl_get_proc_address(hooks::route_proc_address);
    });
}
