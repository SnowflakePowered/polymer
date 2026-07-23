use windows_sys::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Gdi::HDC,
    UI::WindowsAndMessaging::MSG,
};

snowglass_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(snowglass_platform_windows::InlineDetour<_>)]
    #[installer]
    pub(crate) struct ModalHookInstallation;

    #[trampoline]
    #[terminal(installed(target = system_draw_text_w), fallback = 0)]
    #[infix(super::runtime::draw_text_w)]
    pub(super) unsafe extern "system" fn draw_text_w(
        dc: HDC, text: *const u16, count: i32, rect: *mut RECT, format: u32,
    ) -> i32;

    #[trampoline]
    #[terminal(installed(target = system_peek_message_w), fallback = 0)]
    #[infix(super::runtime::peek_message_w)]
    pub(super) unsafe extern "system" fn peek_message_w(
        message: *mut MSG, window: HWND, minimum: u32, maximum: u32, remove: u32,
    ) -> i32;

    #[trampoline]
    #[terminal(installed(target = system_create_compatible_dc), fallback = std::ptr::null_mut())]
    #[infix(super::runtime::create_compatible_dc)]
    pub(super) unsafe extern "system" fn create_compatible_dc(dc: HDC) -> HDC;

    #[trampoline]
    #[terminal(installed(target = system_delete_dc), fallback = 0)]
    #[infix(super::runtime::delete_dc)]
    pub(super) unsafe extern "system" fn delete_dc(dc: HDC) -> i32;

    #[trampoline]
    #[terminal(installed(target = system_enable_window), fallback = 0)]
    #[infix(super::runtime::enable_window)]
    pub(super) unsafe extern "system" fn enable_window(window: HWND, enable: i32) -> i32;
}

#[link(name = "user32")]
unsafe extern "system" {
    #[link_name = "DrawTextW"]
    fn system_draw_text_w(
        dc: HDC,
        text: *const u16,
        count: i32,
        rect: *mut RECT,
        format: u32,
    ) -> i32;

    #[link_name = "PeekMessageW"]
    fn system_peek_message_w(
        message: *mut MSG,
        window: HWND,
        minimum: u32,
        maximum: u32,
        remove: u32,
    ) -> i32;

    #[link_name = "EnableWindow"]
    fn system_enable_window(window: HWND, enable: i32) -> i32;
}

#[link(name = "gdi32")]
unsafe extern "system" {
    #[link_name = "CreateCompatibleDC"]
    fn system_create_compatible_dc(dc: HDC) -> HDC;

    #[link_name = "DeleteDC"]
    fn system_delete_dc(dc: HDC) -> i32;
}
