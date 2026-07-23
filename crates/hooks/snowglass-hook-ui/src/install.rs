use windows_sys::Win32::Foundation::{E_FAIL, HWND};

#[link(name = "user32")]
unsafe extern "system" {
    #[link_name = "MessageBoxW"]
    pub(crate) fn system_message_box_w(
        owner: HWND,
        text: *const u16,
        caption: *const u16,
        style: u32,
    ) -> i32;
    #[link_name = "MessageBoxA"]
    pub(crate) fn system_message_box_a(
        owner: HWND,
        text: *const u8,
        caption: *const u8,
        style: u32,
    ) -> i32;
    #[link_name = "MessageBoxExW"]
    pub(crate) fn system_message_box_ex_w(
        owner: HWND,
        text: *const u16,
        caption: *const u16,
        style: u32,
        language_id: u16,
    ) -> i32;
    #[link_name = "MessageBoxExA"]
    pub(crate) fn system_message_box_ex_a(
        owner: HWND,
        text: *const u8,
        caption: *const u8,
        style: u32,
        language_id: u16,
    ) -> i32;
}

pub(crate) unsafe fn install() -> i32 {
    match crate::hooks::HookInstallation::install() {
        Ok(()) => {
            crate::qt::install();
            0
        }
        Err(_) => E_FAIL,
    }
}
