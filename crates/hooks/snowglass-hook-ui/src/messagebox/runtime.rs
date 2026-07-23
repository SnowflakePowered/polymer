use snowglass_ipc::ui::MessageBoxRequest;
use windows_strings::{PCSTR, PCWSTR};
use windows_sys::Win32::{
    Foundation::HWND,
    System::Threading::{GetCurrentProcessId, GetCurrentThreadId},
};

struct MessageBox {
    owner: HWND,
    text: String,
    caption: String,
    style: u32,
}

impl MessageBox {
    unsafe fn wide(owner: HWND, text: *const u16, caption: *const u16, style: u32) -> Self {
        Self {
            owner,
            text: unsafe { Self::wide_string(text) },
            caption: unsafe { Self::wide_string(caption) },
            style,
        }
    }

    unsafe fn ansi(owner: HWND, text: *const u8, caption: *const u8, style: u32) -> Self {
        Self {
            owner,
            text: unsafe { Self::ansi_string(text) },
            caption: unsafe { Self::ansi_string(caption) },
            style,
        }
    }

    unsafe fn wide_string(pointer: *const u16) -> String {
        if pointer.is_null() {
            String::new()
        } else {
            unsafe { PCWSTR::from_raw(pointer).display().to_string() }
        }
    }

    unsafe fn ansi_string(pointer: *const u8) -> String {
        if pointer.is_null() {
            String::new()
        } else {
            unsafe { PCSTR::from_raw(pointer).display().to_string() }
        }
    }

    fn forward(self) -> Option<i32> {
        let session = snowglass_hook::config::session().ok()?;
        snowglass_ui::forward_message_box(
            &session,
            &MessageBoxRequest {
                protocol_version: snowglass_ipc::ui::UI_PROTOCOL_VERSION,
                process_id: unsafe { GetCurrentProcessId() },
                thread_id: unsafe { GetCurrentThreadId() },
                owner_window: self.owner as i64,
                caption: self.caption,
                text: self.text,
                style: self.style,
                buttons: snowglass_ui::native_buttons(self.style),
                default_result: snowglass_ui::native_default_result(self.style),
                escape_result: snowglass_ui::escape_button(self.style),
            },
        )
    }
}

pub(crate) unsafe fn message_box_w(
    owner: HWND,
    text: *const u16,
    caption: *const u16,
    style: u32,
    mut context: super::hooks::MessageBoxWContext<'_>,
) -> i32 {
    if let Some(result) = unsafe { MessageBox::wide(owner, text, caption, style) }.forward() {
        result
    } else {
        snowglass_hook::next!(context, owner, text, caption, style)
    }
}

pub(crate) unsafe fn message_box_a(
    owner: HWND,
    text: *const u8,
    caption: *const u8,
    style: u32,
    mut context: super::hooks::MessageBoxAContext<'_>,
) -> i32 {
    if let Some(result) = unsafe { MessageBox::ansi(owner, text, caption, style) }.forward() {
        result
    } else {
        snowglass_hook::next!(context, owner, text, caption, style)
    }
}

pub(crate) unsafe fn message_box_ex_w(
    owner: HWND,
    text: *const u16,
    caption: *const u16,
    style: u32,
    language_id: u16,
    mut context: super::hooks::MessageBoxExWContext<'_>,
) -> i32 {
    if let Some(result) = unsafe { MessageBox::wide(owner, text, caption, style) }.forward() {
        result
    } else {
        snowglass_hook::next!(context, owner, text, caption, style, language_id)
    }
}

pub(crate) unsafe fn message_box_ex_a(
    owner: HWND,
    text: *const u8,
    caption: *const u8,
    style: u32,
    language_id: u16,
    mut context: super::hooks::MessageBoxExAContext<'_>,
) -> i32 {
    if let Some(result) = unsafe { MessageBox::ansi(owner, text, caption, style) }.forward() {
        result
    } else {
        snowglass_hook::next!(context, owner, text, caption, style, language_id)
    }
}
