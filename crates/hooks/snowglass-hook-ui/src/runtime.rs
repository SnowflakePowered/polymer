use snowglass_ipc::ui::MessageBoxRequest;
use std::ffi::CStr;
use windows_sys::Win32::{
    Foundation::HWND,
    Globalization::{CP_ACP, MultiByteToWideChar},
    System::Threading::{GetCurrentProcessId, GetCurrentThreadId},
};

const MAX_TEXT_UNITS: usize = 1024 * 1024;

unsafe fn wide_string(pointer: *const u16) -> String {
    if pointer.is_null() {
        return String::new();
    }
    let mut length = 0;
    while length < MAX_TEXT_UNITS && unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(pointer, length) })
}

unsafe fn ansi_string(pointer: *const u8) -> String {
    if pointer.is_null() {
        return String::new();
    }
    let bytes = unsafe { CStr::from_ptr(pointer.cast()) }.to_bytes();
    let Ok(byte_count) = i32::try_from(bytes.len()) else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    let length =
        unsafe { MultiByteToWideChar(CP_ACP, 0, pointer, byte_count, std::ptr::null_mut(), 0) };
    if length <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide = vec![0; length as usize];
    let written =
        unsafe { MultiByteToWideChar(CP_ACP, 0, pointer, byte_count, wide.as_mut_ptr(), length) };
    if written <= 0 {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        String::from_utf16_lossy(&wide[..written as usize])
    }
}

fn forward(owner: HWND, text: String, caption: String, style: u32) -> Option<i32> {
    let session = snowglass_hook::config::session().ok()?;
    snowglass_ui::forward_message_box(
        &session,
        &MessageBoxRequest {
            protocol_version: snowglass_ipc::ui::UI_PROTOCOL_VERSION,
            process_id: unsafe { GetCurrentProcessId() },
            thread_id: unsafe { GetCurrentThreadId() },
            owner_window: owner as i64,
            caption,
            text,
            style,
            buttons: snowglass_ui::native_buttons(style),
            default_result: snowglass_ui::native_default_result(style),
            escape_result: snowglass_ui::escape_button(style),
        },
    )
}

pub(crate) unsafe fn message_box_w(
    owner: HWND,
    text: *const u16,
    caption: *const u16,
    style: u32,
    mut context: crate::hooks::MessageBoxWContext<'_>,
) -> i32 {
    if let Some(result) = forward(
        owner,
        unsafe { wide_string(text) },
        unsafe { wide_string(caption) },
        style,
    ) {
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
    mut context: crate::hooks::MessageBoxAContext<'_>,
) -> i32 {
    if let Some(result) = forward(
        owner,
        unsafe { ansi_string(text) },
        unsafe { ansi_string(caption) },
        style,
    ) {
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
    mut context: crate::hooks::MessageBoxExWContext<'_>,
) -> i32 {
    if let Some(result) = forward(
        owner,
        unsafe { wide_string(text) },
        unsafe { wide_string(caption) },
        style,
    ) {
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
    mut context: crate::hooks::MessageBoxExAContext<'_>,
) -> i32 {
    if let Some(result) = forward(
        owner,
        unsafe { ansi_string(text) },
        unsafe { ansi_string(caption) },
        style,
    ) {
        result
    } else {
        snowglass_hook::next!(context, owner, text, caption, style, language_id)
    }
}
