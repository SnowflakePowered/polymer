use windows_sys::Win32::Foundation::HWND;

polymer_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(polymer_platform_windows::InlineDetour<_>)]
    #[installer]
    pub(crate) struct MessageBoxHookInstallation;

    #[trampoline]
    #[terminal(installed(target = system_message_box_w), fallback = 0)]
    #[infix(super::runtime::message_box_w)]
    pub(super) unsafe extern "system" fn message_box_w(
        owner: HWND, text: *const u16, caption: *const u16, style: u32,
    ) -> i32;

    #[trampoline]
    #[terminal(installed(target = system_message_box_a), fallback = 0)]
    #[infix(super::runtime::message_box_a)]
    pub(super) unsafe extern "system" fn message_box_a(
        owner: HWND, text: *const u8, caption: *const u8, style: u32,
    ) -> i32;
}

#[link(name = "user32")]
unsafe extern "system" {
    #[link_name = "MessageBoxW"]
    fn system_message_box_w(owner: HWND, text: *const u16, caption: *const u16, style: u32) -> i32;

    #[link_name = "MessageBoxA"]
    fn system_message_box_a(owner: HWND, text: *const u8, caption: *const u8, style: u32) -> i32;
}
