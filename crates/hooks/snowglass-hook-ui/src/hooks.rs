use windows_sys::Win32::Foundation::HWND;

snowglass_hook::chain_hooks! {
    #![order(reverse)]
    #![detour(snowglass_platform_windows::InlineDetour<_>)]
    #[installer]
    pub(crate) struct HookInstallation;

    #[trampoline]
    #[terminal(installed(target = crate::install::system_message_box_w), fallback = 0)]
    #[infix(crate::runtime::message_box_w)]
    pub(crate) unsafe extern "system" fn message_box_w(
        owner: HWND, text: *const u16, caption: *const u16, style: u32,
    ) -> i32;

    #[trampoline]
    #[terminal(installed(target = crate::install::system_message_box_a), fallback = 0)]
    #[infix(crate::runtime::message_box_a)]
    pub(crate) unsafe extern "system" fn message_box_a(
        owner: HWND, text: *const u8, caption: *const u8, style: u32,
    ) -> i32;

    #[trampoline]
    #[terminal(installed(target = crate::install::system_message_box_ex_w), fallback = 0)]
    #[infix(crate::runtime::message_box_ex_w)]
    pub(crate) unsafe extern "system" fn message_box_ex_w(
        owner: HWND, text: *const u16, caption: *const u16, style: u32, language_id: u16,
    ) -> i32;

    #[trampoline]
    #[terminal(installed(target = crate::install::system_message_box_ex_a), fallback = 0)]
    #[infix(crate::runtime::message_box_ex_a)]
    pub(crate) unsafe extern "system" fn message_box_ex_a(
        owner: HWND, text: *const u8, caption: *const u8, style: u32, language_id: u16,
    ) -> i32;
}
