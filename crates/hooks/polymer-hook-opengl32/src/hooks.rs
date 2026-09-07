//! Typed OpenGL hook declarations and capture callbacks.

use std::ffi::c_void;

pub(crate) type Hdc = *mut c_void;

polymer_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(polymer_platform_windows::InlineDetour<_>)]
    #[installer(state = ())]
    pub(crate) struct HookInstallation;

    #[trampoline]
    #[terminal(installed(target = crate::install::system_delete_context), fallback = 0)]
    #[prefix(|_context| unsafe { polymer_producer_opengl::before_delete_context(rendering_context as usize); })]
    #[postfix(|result, _context| {
        if result != 0 {
            unsafe { polymer_producer_opengl::after_delete_context(rendering_context as usize); }
        }
    })]
    pub(crate) unsafe extern "system" fn delete_context(rendering_context: *mut c_void) -> i32;

    #[trampoline]
    #[terminal(installed(
        target = crate::install::system_swap_buffers,
    ), fallback = 0)]
    #[prefix(|_context| unsafe { polymer_producer_opengl::before_swap_buffers(hdc as isize); })]
    pub(crate) unsafe extern "system" fn swap_buffers(hdc: Hdc) -> i32;

    #[trampoline]
    #[terminal(installed(
        target = crate::install::system_swap_layer_buffers,
    ), fallback = 0)]
    #[prefix(|_context| if planes & 1 != 0 { unsafe { polymer_producer_opengl::before_swap_buffers(hdc as isize); } })]
    pub(crate) unsafe extern "system" fn swap_layer_buffers(hdc: Hdc, planes: u32) -> i32;

}
