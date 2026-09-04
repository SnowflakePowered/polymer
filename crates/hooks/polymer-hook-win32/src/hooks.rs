//! Concrete Win32 input hook-chain declarations.

use windows_sys::Win32::Foundation::POINT;

polymer_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(polymer_platform_windows::InlineDetour<_>)]
    #[installer(state = crate::runtime::Runtime)]
    pub(crate) struct HookInstallation;

    #[trampoline]
    #[terminal(installed(
        target = windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos,
    ), fallback = 0)]
    #[infix(crate::runtime::get_cursor_pos)]
    pub unsafe extern "system" fn get_cursor_pos(point: *mut POINT) -> i32;
}
