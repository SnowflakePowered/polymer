use polymer_ipc::ui::{MessageBoxRequest, UI_PROTOCOL_VERSION};
use windows_strings::{PCSTR, PCWSTR};
use windows_sys::Win32::{
    Foundation::HWND,
    System::Threading::{GetCurrentProcessId, GetCurrentThreadId},
};

struct NativeMessage {
    owner: HWND,
    text: String,
    caption: String,
    style: u32,
}

impl NativeMessage {
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

    unsafe fn wide_string(value: *const u16) -> String {
        if value.is_null() {
            String::new()
        } else {
            unsafe { PCWSTR::from_raw(value).display().to_string() }
        }
    }

    unsafe fn ansi_string(value: *const u8) -> String {
        if value.is_null() {
            String::new()
        } else {
            unsafe { PCSTR::from_raw(value).display().to_string() }
        }
    }

    fn forward(&self) -> Option<i32> {
        let buttons = polymer_ui::native_buttons(self.style);
        let dismiss = polymer_ui::escape_button(self.style)
            .or_else(|| polymer_ui::native_default_result(self.style))
            .or_else(|| buttons.first().map(|button| button.result))?;
        let request = MessageBoxRequest {
            protocol_version: UI_PROTOCOL_VERSION,
            process_id: unsafe { GetCurrentProcessId() },
            thread_id: unsafe { GetCurrentThreadId() },
            owner_window: self.owner as i64,
            caption: self.caption.clone(),
            text: self.text.clone(),
            style: self.style,
            buttons,
            default_result: Some(dismiss),
            escape_result: Some(dismiss),
        };
        let session = polymer_hook::config::session().ok()?;
        polymer_ui::forward_message_box(&session, &request)
    }
}

pub(super) unsafe fn message_box_w(
    owner: HWND,
    text: *const u16,
    caption: *const u16,
    style: u32,
    mut context: super::hooks::MessageBoxWContext<'_>,
) -> i32 {
    let message = unsafe { NativeMessage::wide(owner, text, caption, style) };
    message
        .forward()
        .unwrap_or_else(|| polymer_hook::next!(context, owner, text, caption, style))
}

pub(super) unsafe fn message_box_a(
    owner: HWND,
    text: *const u8,
    caption: *const u8,
    style: u32,
    mut context: super::hooks::MessageBoxAContext<'_>,
) -> i32 {
    let message = unsafe { NativeMessage::ansi(owner, text, caption, style) };
    message
        .forward()
        .unwrap_or_else(|| polymer_hook::next!(context, owner, text, caption, style))
}
