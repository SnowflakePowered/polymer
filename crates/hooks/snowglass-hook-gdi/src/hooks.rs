//! Typed GDI hook declarations.

use windows_sys::Win32::{
    Foundation::HWND,
    Graphics::Gdi::{HDC, PAINTSTRUCT},
};

snowglass_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(snowglass_platform_windows::InlineDetour<_>)]
    #[installer]
    pub(crate) struct HookInstallation;

    #[trampoline]
    #[terminal(installed(
        target = crate::install::system_begin_paint,
    ), fallback = std::ptr::null_mut())]
    #[infix(crate::runtime::redirect_begin_paint)]
    pub(crate) unsafe extern "system" fn begin_paint(
        hwnd: HWND,
        paint: *mut PAINTSTRUCT,
    ) -> HDC;

    #[trampoline]
    #[terminal(installed(
        target = crate::install::system_end_paint,
    ), fallback = 0)]
    #[infix(crate::runtime::redirect_end_paint)]
    pub(crate) unsafe extern "system" fn end_paint(
        hwnd: HWND,
        paint: *const PAINTSTRUCT,
    ) -> i32;

    #[trampoline]
    #[terminal(installed(
        target = crate::install::system_bit_blt,
    ), fallback = 0)]
    #[infix(crate::runtime::redirect_bit_blt)]
    pub(crate) unsafe extern "system" fn bit_blt(
        destination: HDC, x: i32, y: i32, width: i32, height: i32,
        source: HDC, source_x: i32, source_y: i32, rop: u32,
    ) -> i32;
}
