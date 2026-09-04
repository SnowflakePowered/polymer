use parking_lot::Mutex;
use polymer_ipc::ui::{DialogButton, MessageBoxRequest, UI_PROTOCOL_VERSION};
use std::{
    collections::HashMap,
    sync::OnceLock,
};
use windows_strings::PCWSTR;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, RECT, WPARAM},
    Graphics::Gdi::{HDC, WindowFromDC},
    System::Threading::{GetCurrentProcessId, GetCurrentThreadId},
    UI::WindowsAndMessaging::{
        BM_CLICK, EnumChildWindows, EnumThreadWindows, GA_ROOT, GWL_STYLE, GetAncestor,
        GetClassNameW, GetWindowLongW, GetWindowTextW, IsWindowVisible, MSG, SendMessageW,
        WS_DISABLED,
    },
};

#[derive(Default)]
struct Candidate {
    text: Vec<String>,
    buttons: Vec<(HWND, String)>,
}

struct Discovery {
    disabled: HWND,
    dialog: HWND,
}

unsafe impl Send for Discovery {}

// HWND values are process-local and only accessed from UI hook callbacks.
unsafe impl Send for Candidate {}

static CANDIDATES: OnceLock<Mutex<HashMap<isize, Candidate>>> = OnceLock::new();
static MEMORY_DCS: OnceLock<Mutex<HashMap<isize, isize>>> = OnceLock::new();

fn candidates() -> &'static Mutex<HashMap<isize, Candidate>> {
    CANDIDATES.get_or_init(Default::default)
}

fn memory_dcs() -> &'static Mutex<HashMap<isize, isize>> {
    MEMORY_DCS.get_or_init(Default::default)
}

unsafe fn window_class(window: HWND) -> String {
    let mut value = [0u16; 64];
    let length = unsafe { GetClassNameW(window, value.as_mut_ptr(), value.len() as i32) };
    String::from_utf16_lossy(&value[..length.max(0) as usize])
}

unsafe fn drawn_text(text: *const u16, count: i32) -> String {
    if text.is_null() {
        return String::new();
    }
    if count < 0 {
        unsafe { PCWSTR::from_raw(text).display().to_string() }
    } else {
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, count as usize) })
    }
}

pub(super) unsafe fn draw_text_w(
    dc: HDC,
    text: *const u16,
    count: i32,
    rect: *mut RECT,
    format: u32,
    mut context: super::hooks::DrawTextWContext<'_>,
) -> i32 {
    let result = polymer_hook::next!(context, dc, text, count, rect, format);
    let window = unsafe { WindowFromDC(dc) };
    let window = if window.is_null() {
        memory_dcs()
            .lock()
            .get(&(dc as isize))
            .copied()
            .unwrap_or_default() as HWND
    } else {
        window
    };
    let root = unsafe { GetAncestor(window, GA_ROOT) };
    if window.is_null() || root.is_null() || window == root {
        return result;
    }
    let class = unsafe { window_class(window) };
    let is_button = class.eq_ignore_ascii_case("button");
    if !is_button && class != "hiroWidget" {
        return result;
    }
    let label = unsafe { drawn_text(text, count) }.trim().to_owned();
    if label.is_empty() {
        return result;
    }
    let mut dialogs = candidates().lock();
    let candidate = dialogs.entry(root as isize).or_default();
    if is_button {
        if !candidate
            .buttons
            .iter()
            .any(|(handle, _)| *handle == window)
        {
            candidate.buttons.push((window, label));
        }
    } else if !candidate.text.contains(&label) {
        candidate.text.push(label);
    }
    result
}

pub(super) unsafe fn create_compatible_dc(
    dc: HDC,
    mut context: super::hooks::CreateCompatibleDcContext<'_>,
) -> HDC {
    let compatible = polymer_hook::next!(context, dc);
    let window = unsafe { WindowFromDC(dc) };
    if !compatible.is_null() && !window.is_null() {
        memory_dcs()
            .lock()
            .insert(compatible as isize, window as isize);
    }
    compatible
}

pub(super) unsafe fn delete_dc(dc: HDC, mut context: super::hooks::DeleteDcContext<'_>) -> i32 {
    memory_dcs().lock().remove(&(dc as isize));
    polymer_hook::next!(context, dc)
}

unsafe extern "system" fn count_buttons(window: HWND, parameter: LPARAM) -> i32 {
    if unsafe { window_class(window) }.eq_ignore_ascii_case("button")
        && unsafe { IsWindowVisible(window) } != 0
    {
        unsafe { *(parameter as *mut usize) += 1 };
    }
    1
}

unsafe extern "system" fn discover_top_level(window: HWND, parameter: LPARAM) -> i32 {
    let discovery = unsafe { &mut *(parameter as *mut Discovery) };
    if window != discovery.disabled
        && unsafe { IsWindowVisible(window) } != 0
        && unsafe { GetWindowLongW(window, GWL_STYLE) as u32 & WS_DISABLED } == 0
    {
        let mut button_count = 0usize;
        unsafe {
            EnumChildWindows(
                window,
                Some(count_buttons),
                &mut button_count as *mut _ as LPARAM,
            )
        };
        if (1..=3).contains(&button_count) {
            discovery.dialog = window;
            return 0;
        }
    }
    1
}

unsafe extern "system" fn collect_native_child(window: HWND, parameter: LPARAM) -> i32 {
    let candidate = unsafe { &mut *(parameter as *mut Candidate) };
    let mut value = [0u16; 4096];
    let length = unsafe { GetWindowTextW(window, value.as_mut_ptr(), value.len() as i32) };
    if length <= 0 {
        return 1;
    }
    let text = String::from_utf16_lossy(&value[..length as usize]);
    if unsafe { window_class(window) }.eq_ignore_ascii_case("button") {
        candidate.buttons.push((window, text));
    } else if !candidate.text.contains(&text) {
        candidate.text.push(text);
    }
    1
}

pub(super) unsafe fn enable_window(
    window: HWND,
    enable: i32,
    mut context: super::hooks::EnableWindowContext<'_>,
) -> i32 {
    let result = polymer_hook::next!(context, window, enable);
    if enable == 0 {
        let mut discovery = Discovery {
            disabled: window,
            dialog: std::ptr::null_mut(),
        };
        unsafe {
            EnumThreadWindows(
                GetCurrentThreadId(),
                Some(discover_top_level),
                &mut discovery as *mut _ as LPARAM,
            )
        };
        if !discovery.dialog.is_null() {
            let mut candidate = Candidate::default();
            unsafe {
                EnumChildWindows(
                    discovery.dialog,
                    Some(collect_native_child),
                    &mut candidate as *mut _ as LPARAM,
                )
            };
            let mut dialogs = candidates().lock();
            let existing = dialogs.entry(discovery.dialog as isize).or_default();
            for text in candidate.text {
                if !existing.text.contains(&text) {
                    existing.text.push(text);
                }
            }
            for button in candidate.buttons {
                if !existing.buttons.iter().any(|value| value.0 == button.0) {
                    existing.buttons.push(button);
                }
            }
        }
    }
    result
}

unsafe extern "system" fn find_disabled_window(window: HWND, parameter: LPARAM) -> i32 {
    let state = unsafe { &mut *(parameter as *mut (HWND, bool)) };
    if window != state.0
        && unsafe { IsWindowVisible(window) } != 0
        && unsafe { GetWindowLongW(window, GWL_STYLE) as u32 & WS_DISABLED } != 0
    {
        state.1 = true;
        return 0;
    }
    1
}

fn ready_dialog() -> Option<(HWND, Candidate)> {
    let mut dialogs = candidates().lock();
    let key = dialogs.iter().find_map(|(key, candidate)| {
        let root = *key as HWND;
        let mut disabled = (root, false);
        unsafe {
            EnumThreadWindows(
                GetCurrentThreadId(),
                Some(find_disabled_window),
                &mut disabled as *mut _ as LPARAM,
            )
        };
        if unsafe { IsWindowVisible(root) } == 0 || !disabled.1 || candidate.text.is_empty() {
            return None;
        }
        let mut button_count = 0usize;
        unsafe {
            EnumChildWindows(
                root,
                Some(count_buttons),
                &mut button_count as *mut _ as LPARAM,
            )
        };
        (button_count != 0 && candidate.buttons.len() == button_count).then_some(*key)
    })?;
    dialogs
        .remove(&key)
        .map(|candidate| (key as HWND, candidate))
}

unsafe fn forward(root: HWND, candidate: Candidate) {
    let buttons = candidate
        .buttons
        .iter()
        .enumerate()
        .map(|(index, (_, label))| DialogButton {
            result: index as i32,
            label: label.clone(),
        })
        .collect::<Vec<_>>();
    let fallback = buttons.last().map(|button| button.result);
    let mut caption = [0u16; 256];
    let caption_length =
        unsafe { GetWindowTextW(root, caption.as_mut_ptr(), caption.len() as i32) };
    let request = MessageBoxRequest {
        protocol_version: UI_PROTOCOL_VERSION,
        process_id: unsafe { GetCurrentProcessId() },
        thread_id: unsafe { GetCurrentThreadId() },
        owner_window: root as i64,
        caption: String::from_utf16_lossy(&caption[..caption_length.max(0) as usize]),
        text: candidate.text.join("\n\n"),
        style: 0,
        buttons,
        default_result: fallback,
        escape_result: fallback,
    };
    let Some(result) = polymer_hook::config::session()
        .ok()
        .and_then(|session| polymer_ui::forward_message_box(&session, &request))
    else {
        return;
    };
    if let Some((button, _)) = candidate.buttons.get(result as usize) {
        unsafe { SendMessageW(*button, BM_CLICK, 0 as WPARAM, 0 as LPARAM) };
    }
}

pub(super) unsafe fn peek_message_w(
    message: *mut MSG,
    window: HWND,
    minimum: u32,
    maximum: u32,
    remove: u32,
    mut context: super::hooks::PeekMessageWContext<'_>,
) -> i32 {
    if let Some((root, candidate)) = ready_dialog() {
        unsafe { forward(root, candidate) };
    }
    polymer_hook::next!(context, message, window, minimum, maximum, remove)
}
