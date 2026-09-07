//! Typed OpenGL hook declarations and capture callbacks.

use polymer_producer_opengl::WglSwapchain;
use std::ffi::c_void;

pub(crate) type Hdc = *mut c_void;

polymer_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(polymer_platform_windows::InlineDetour<_>)]
    #[installer(state = ())]
    pub(crate) struct HookInstallation;

    #[trampoline]
    #[terminal(installed(target = crate::install::system_delete_context), fallback = 0)]
    #[prefix(|_context| WglSwapchain::native(|| unsafe { polymer_producer_opengl::before_delete_context(rendering_context as usize); }))]
    #[postfix(|result, _context| {
        if result != 0 {
            WglSwapchain::native(|| unsafe { polymer_producer_opengl::after_delete_context(rendering_context as usize); });
            WglSwapchain::context_destroyed(rendering_context as usize);
        }
    })]
    pub(crate) unsafe extern "system" fn delete_context(rendering_context: *mut c_void) -> i32;

    #[trampoline]
    #[terminal(installed(
        target = crate::install::system_swap_buffers,
    ), fallback = 0)]
    #[infix(Callbacks::present)]
    pub(crate) unsafe extern "system" fn swap_buffers(hdc: Hdc) -> i32;

    #[trampoline]
    #[terminal(installed(
        target = crate::install::system_swap_layer_buffers,
    ), fallback = 0)]
    #[infix(Callbacks::present_layers)]
    pub(crate) unsafe extern "system" fn swap_layer_buffers(hdc: Hdc, planes: u32) -> i32;

    #[trampoline]
    #[terminal(installed(target = crate::install::system_make_current), fallback = 0)]
    #[infix(Callbacks::bind)]
    pub(crate) unsafe extern "system" fn make_current(hdc: Hdc, context: *mut c_void) -> i32;

    #[trampoline]
    #[terminal(installed(target = crate::install::system_get_current_dc), fallback = std::ptr::null_mut())]
    #[infix(Callbacks::current_dc)]
    pub(crate) unsafe extern "system" fn get_current_dc() -> Hdc;

    #[trampoline]
    #[terminal(installed(target = crate::install::system_get_proc_address), fallback = std::ptr::null())]
    #[infix(Callbacks::procedure)]
    pub(crate) unsafe extern "system" fn get_proc_address(name: *const u8) -> *const c_void;

    #[trampoline]
    #[terminal(installed(target = crate::install::system_get_client_rect), fallback = 0)]
    #[postfix(|result, _context| {
        if result != 0 && !rect.is_null() {
            if let Some([width, height]) = WglSwapchain::window_extent(window as isize) {
                unsafe { *rect = windows_sys::Win32::Foundation::RECT { left: 0, top: 0, right: width as i32, bottom: height as i32 }; }
            }
        }
    })]
    pub(crate) unsafe extern "system" fn get_client_rect(window: *mut c_void, rect: *mut windows_sys::Win32::Foundation::RECT) -> i32;
}

struct Callbacks;
impl Callbacks {
    unsafe fn present(hdc: Hdc, mut next: SwapBuffersContext<'_>) -> i32 {
        let dc = unsafe { WglSwapchain::present(hdc) };
        if dc.is_null() {
            return 0;
        }
        WglSwapchain::native(|| polymer_hook::next!(next, dc))
    }
    unsafe fn present_layers(hdc: Hdc, planes: u32, mut next: SwapLayerBuffersContext<'_>) -> i32 {
        let dc = if planes & 1 != 0 {
            unsafe { WglSwapchain::present(hdc) }
        } else {
            hdc
        };
        if dc.is_null() {
            return 0;
        }
        WglSwapchain::native(|| polymer_hook::next!(next, dc, planes))
    }
    unsafe fn bind(hdc: Hdc, context: *mut c_void, mut next: MakeCurrentContext<'_>) -> i32 {
        unsafe {
            WglSwapchain::make_current(hdc, hdc, context, |draw, _, context| {
                polymer_hook::next!(next, draw, context)
            })
        }
    }
    unsafe fn current_dc(mut next: GetCurrentDcContext<'_>) -> Hdc {
        WglSwapchain::current_dc(polymer_hook::next!(next), false)
    }
    unsafe fn procedure(name: *const u8, mut next: GetProcAddressContext<'_>) -> *const c_void {
        let address = polymer_hook::next!(next, name);
        unsafe { WglSwapchain::procedure(name, address) }
    }
}
