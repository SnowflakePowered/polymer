//! Concrete Win32 input hook-chain declarations.

use windows_sys::Win32::{
    Foundation::POINT,
    UI::Input::{HRAWINPUT, RAWINPUTDEVICE},
};

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

    #[trampoline]
    #[terminal(installed(
        target = windows_sys::Win32::UI::Input::RegisterRawInputDevices,
    ), fallback = 0)]
    #[infix(crate::runtime::register_raw_input_devices)]
    pub unsafe extern "system" fn register_raw_input_devices(
        devices: *const RAWINPUTDEVICE,
        count: u32,
        size: u32,
    ) -> i32;

    #[trampoline]
    #[terminal(installed(
        target = windows_sys::Win32::UI::Input::GetRawInputData,
    ), fallback = u32::MAX)]
    #[infix(crate::runtime::get_raw_input_data)]
    pub unsafe extern "system" fn get_raw_input_data(
        input: HRAWINPUT,
        command: u32,
        data: *mut core::ffi::c_void,
        size: *mut u32,
        header_size: u32,
    ) -> u32;
}
