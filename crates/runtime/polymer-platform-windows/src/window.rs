use crate::{NativeWindow, ProcessId};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};

fn hwnd(window: NativeWindow) -> HWND {
    HWND(window.0 as *mut _)
}

unsafe fn post_message(window: NativeWindow, message: u32, wparam: usize, lparam: isize) {
    let _ = unsafe {
        windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            Some(hwnd(window)),
            message,
            WPARAM(wparam),
            LPARAM(lparam),
        )
    };
}
#[cfg(windows)]
pub struct WindowKeyForwarder {
    viewer: HWND,
    context: *mut std::sync::atomic::AtomicIsize,
}

#[cfg(windows)]
impl WindowKeyForwarder {
    pub fn install(
        viewer: NativeWindow,
        backing_window: std::sync::Arc<std::sync::atomic::AtomicIsize>,
    ) -> std::io::Result<Self> {
        use windows::Win32::UI::Shell::SetWindowSubclass;
        let context =
            std::sync::Arc::into_raw(backing_window) as *mut std::sync::atomic::AtomicIsize;
        let viewer = hwnd(viewer);
        // SAFETY: context remains allocated until Drop removes this exact
        // subclass, and the viewer HWND is live while the forwarder is held.
        if unsafe {
            SetWindowSubclass(
                viewer,
                Some(key_forward_subclass),
                context as usize,
                context as usize,
            )
        } == false
        {
            // SAFETY: installation failed, so no callback owns this Arc.
            unsafe { drop(std::sync::Arc::from_raw(context)) };
            Err(std::io::Error::last_os_error())
        } else {
            Ok(Self { viewer, context })
        }
    }
}

#[cfg(windows)]
impl Drop for WindowKeyForwarder {
    fn drop(&mut self) {
        use windows::Win32::UI::Shell::RemoveWindowSubclass;
        // SAFETY: this removes the callback before releasing its context.
        unsafe {
            let _ = RemoveWindowSubclass(
                self.viewer,
                Some(key_forward_subclass),
                self.context as usize,
            );
            drop(std::sync::Arc::from_raw(self.context));
        }
    }
}

#[cfg(windows)]
unsafe extern "system" fn key_forward_subclass(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass_id: usize,
    context: usize,
) -> windows::Win32::Foundation::LRESULT {
    use std::sync::atomic::Ordering;
    use windows::Win32::UI::{
        Shell::DefSubclassProc,
        WindowsAndMessaging::{
            PostMessageW, SIZE_MINIMIZED, WM_CHAR, WM_DEADCHAR, WM_KEYDOWN, WM_KEYUP, WM_SIZE,
            WM_SYSCHAR, WM_SYSDEADCHAR, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_UNICHAR,
        },
    };
    // SAFETY: SetWindowSubclass supplies the live Arc allocation above.
    let target =
        unsafe { &*(context as *const std::sync::atomic::AtomicIsize) }.load(Ordering::Acquire);

    // Resizing the foreground viewer deactivates the hidden source window. Restore
    // its logical focus without showing it or changing its size.
    if message == WM_SIZE && wparam.0 != SIZE_MINIMIZED as usize && target != 0 {
        unsafe { post_logical_focus(target) };
    }
    if matches!(
        message,
        WM_KEYDOWN
            | WM_KEYUP
            | WM_SYSKEYDOWN
            | WM_SYSKEYUP
            | WM_CHAR
            | WM_DEADCHAR
            | WM_SYSCHAR
            | WM_SYSDEADCHAR
            | WM_UNICHAR
    ) {
        if target != 0 {
            if matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN) {
                unsafe { post_logical_focus(target) };
            }
            let _ = unsafe { PostMessageW(Some(HWND(target as *mut _)), message, wparam, lparam) };
        }
    }
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

#[cfg(windows)]
unsafe fn post_logical_focus(target: isize) {
    use windows::Win32::UI::WindowsAndMessaging::{
        WA_ACTIVE, WM_ACTIVATE, WM_ACTIVATEAPP, WM_SETFOCUS,
    };
    unsafe {
        let window = NativeWindow(target);
        post_message(window, WM_ACTIVATEAPP, 1, 0);
        post_message(window, WM_ACTIVATE, WA_ACTIVE as usize, 0);
        post_message(window, WM_SETFOCUS, 0, 0);
    }
}

#[cfg(windows)]
pub fn forward_focus(window: NativeWindow, focused: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{
        WA_INACTIVE, WM_ACTIVATE, WM_ACTIVATEAPP, WM_KILLFOCUS,
    };
    unsafe {
        if focused {
            post_logical_focus(window.0);
        } else {
            post_message(window, WM_KILLFOCUS, 0, 0);
            post_message(window, WM_ACTIVATE, WA_INACTIVE as usize, 0);
            post_message(window, WM_ACTIVATEAPP, 0, 0);
        }
    }
}

#[cfg(windows)]
pub fn find_main_window(process_id: ProcessId) -> Option<NativeWindow> {
    use windows::Win32::{
        Foundation::RECT,
        UI::WindowsAndMessaging::{EnumWindows, GetClientRect, GetWindowThreadProcessId},
    };

    struct Context {
        process_id: u32,
        best: HWND,
        score: i64,
    }
    unsafe extern "system" fn callback(window: HWND, parameter: LPARAM) -> windows::core::BOOL {
        let context = unsafe { &mut *(parameter.0 as *mut Context) };
        let mut owner = 0;
        unsafe { GetWindowThreadProcessId(window, Some(&mut owner)) };
        if owner == context.process_id {
            let mut rect = RECT::default();
            if unsafe { GetClientRect(window, &mut rect) }.is_ok() {
                let area = i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top);
                let style = unsafe {
                    windows::Win32::UI::WindowsAndMessaging::GetWindowLongW(
                        window,
                        windows::Win32::UI::WindowsAndMessaging::GWL_STYLE,
                    )
                } as u32;
                let borderless = style & windows::Win32::UI::WindowsAndMessaging::WS_CAPTION.0 == 0;
                let score = area + if borderless { 1i64 << 60 } else { 0 };
                if score > context.score {
                    context.best = window;
                    context.score = score;
                }
            }
        }
        true.into()
    }
    let mut context = Context {
        process_id: process_id.0,
        best: HWND::default(),
        score: 0,
    };
    let _ = unsafe { EnumWindows(Some(callback), LPARAM((&raw mut context) as isize)) };
    (!context.best.0.is_null()).then_some(NativeWindow(context.best.0 as isize))
}

#[cfg(windows)]
pub fn is_borderless_window(window: NativeWindow) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GWL_STYLE, GetWindowLongW, WS_CAPTION};
    let style = unsafe { GetWindowLongW(hwnd(window), GWL_STYLE) } as u32;
    style & WS_CAPTION.0 == 0
}

#[cfg(windows)]
pub fn client_size(window: NativeWindow) -> Option<(u32, u32)> {
    let mut rect = windows::Win32::Foundation::RECT::default();
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd(window), &mut rect) }
        .is_ok()
        .then_some((
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        ))
}

#[cfg(windows)]
pub fn window_title(window: NativeWindow) -> Option<String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GA_ROOT, GetAncestor, GetWindowTextLengthW, GetWindowTextW,
    };

    fn read(hwnd: HWND) -> Option<String> {
        let length = unsafe { GetWindowTextLengthW(hwnd) };
        if length < 0 {
            return None;
        }
        let mut title = vec![0u16; length as usize + 1];
        let copied = unsafe { GetWindowTextW(hwnd, &mut title) };
        if copied == 0 && length != 0 {
            return None;
        }
        title.truncate(copied as usize);
        Some(String::from_utf16_lossy(&title))
    }

    let child = hwnd(window);
    let root = unsafe { GetAncestor(child, GA_ROOT) };
    (!root.0.is_null())
        .then(|| read(root))
        .flatten()
        .filter(|title| !title.is_empty())
        .or_else(|| read(child))
}

#[cfg(windows)]
pub fn is_process_window(window: NativeWindow, process_id: ProcessId) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow};
    let hwnd = hwnd(window);
    if unsafe { IsWindow(Some(hwnd)) } == false {
        return false;
    }
    let mut owner = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut owner)) };
    owner == process_id.0
}

#[cfg(windows)]
pub fn resize_client(window: NativeWindow, width: u32, height: u32) -> std::io::Result<()> {
    use windows::Win32::{
        Foundation::RECT,
        UI::WindowsAndMessaging::{
            AdjustWindowRectEx, GWL_EXSTYLE, GWL_STYLE, GetWindowLongW, SWP_HIDEWINDOW, SWP_NOMOVE,
            SWP_NOZORDER, SetWindowPos,
        },
    };
    if width == 0 || height == 0 {
        return Ok(());
    }
    let hwnd = hwnd(window);
    let mut client = RECT::default();
    if unsafe { windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client) }.is_ok()
        && (client.right - width as i32).abs() <= 2
        && (client.bottom - height as i32).abs() <= 2
    {
        return Ok(());
    }
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    let ex_style = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width as i32,
        bottom: height as i32,
    };
    unsafe {
        AdjustWindowRectEx(
            &mut rect,
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(style),
            false,
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(ex_style),
        )
    }
    .map_err(|_| std::io::Error::last_os_error())?;
    unsafe {
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOMOVE | SWP_NOZORDER | SWP_HIDEWINDOW,
        )
    }
    .map_err(|_| std::io::Error::last_os_error())
}

#[cfg(windows)]
pub fn forward_key(window: NativeWindow, virtual_key: u32, pressed: bool, repeat: bool) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC_EX, MapVirtualKeyW};
    use windows::Win32::UI::WindowsAndMessaging::{WM_KEYDOWN, WM_KEYUP};
    let message = if pressed { WM_KEYDOWN } else { WM_KEYUP };
    let mapped = unsafe { MapVirtualKeyW(virtual_key, MAPVK_VK_TO_VSC_EX) };
    let scan_code = mapped & 0xff;
    let prefix = (mapped >> 8) & 0xff;
    let mut lparam = 1isize | ((scan_code as isize) << 16);
    if prefix == 0xe0 || prefix == 0xe1 {
        lparam |= 1 << 24;
    }
    if repeat || !pressed {
        lparam |= 1 << 30;
    }
    if !pressed {
        lparam |= 1 << 31;
    }
    unsafe { post_message(window, message, virtual_key as usize, lparam) };
}

#[cfg(windows)]
pub fn keyboard_code(code: &str) -> Option<(u32, u16, bool)> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{MAPVK_VK_TO_VSC_EX, MapVirtualKeyW};
    let virtual_key = match code {
        "Alt" => 0x12,
        "Control" => 0x11,
        "Shift" => 0x10,
        "Meta" => 0x5b,
        "Enter" => 0x0d,
        "Escape" => 0x1b,
        "Tab" => 0x09,
        "Space" => 0x20,
        "Backspace" => 0x08,
        "Delete" => 0x2e,
        "Home" => 0x24,
        "End" => 0x23,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "ArrowLeft" => 0x25,
        "ArrowUp" => 0x26,
        "ArrowRight" => 0x27,
        "ArrowDown" => 0x28,
        "Period" => 0xbe,
        value
            if value.len() == 4
                && value.starts_with("Key")
                && value.as_bytes()[3].is_ascii_uppercase() =>
        {
            value.as_bytes()[3] as u32
        }
        value
            if value.len() == 6
                && value.starts_with("Digit")
                && value.as_bytes()[5].is_ascii_digit() =>
        {
            value.as_bytes()[5] as u32
        }
        value if value.starts_with('F') => value[1..]
            .parse::<u32>()
            .ok()
            .filter(|number| (1..=24).contains(number))
            .map(|number| 0x6f + number)?,
        value if value.starts_with("VK_") => u32::from_str_radix(&value[3..], 16)
            .ok()
            .filter(|key| *key <= u16::MAX.into())?,
        _ => return None,
    };
    let mapped = unsafe { MapVirtualKeyW(virtual_key, MAPVK_VK_TO_VSC_EX) };
    Some((
        virtual_key,
        (mapped & 0xff) as u16,
        matches!((mapped >> 8) & 0xff, 0xe0 | 0xe1),
    ))
}

/// Forwards a physical keyboard transition without losing the scan-code prefix
/// supplied by Raw Input. This is important for navigation keys, whose E0
/// prefix distinguishes them from their numeric-keypad counterparts.
#[cfg(windows)]
pub fn forward_raw_key(
    window: NativeWindow,
    virtual_key: u32,
    scan_code: u16,
    extended: bool,
    pressed: bool,
    repeat: bool,
) {
    use windows::Win32::UI::WindowsAndMessaging::{WM_KEYDOWN, WM_KEYUP};
    let message = if pressed { WM_KEYDOWN } else { WM_KEYUP };
    let mut lparam = 1isize | (((scan_code & 0xff) as isize) << 16);
    if extended {
        lparam |= 1 << 24;
    }
    if repeat || !pressed {
        lparam |= 1 << 30;
    }
    if !pressed {
        lparam |= 1 << 31;
    }
    unsafe { post_message(window, message, virtual_key as usize, lparam) };
}

#[cfg(windows)]
pub fn forward_character(window: NativeWindow, character: char) {
    use windows::Win32::UI::WindowsAndMessaging::WM_CHAR;
    let mut units = [0; 2];
    for unit in character.encode_utf16(&mut units) {
        unsafe { post_message(window, WM_CHAR, *unit as usize, 1) };
    }
}

#[cfg(windows)]
pub fn forward_mouse_move(window: NativeWindow, x: i32, y: i32) {
    use windows::Win32::UI::WindowsAndMessaging::WM_MOUSEMOVE;
    let lparam = ((y as u16 as u32) << 16 | x as u16 as u32) as isize;
    unsafe { post_message(window, WM_MOUSEMOVE, 0, lparam) };
}

#[cfg(windows)]
pub fn forward_mouse_button(window: NativeWindow, button: u16, pressed: bool, x: i32, y: i32) {
    use windows::Win32::UI::WindowsAndMessaging::{
        WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP,
        WM_XBUTTONDOWN, WM_XBUTTONUP,
    };
    let message = match (button, pressed) {
        (1, true) => WM_LBUTTONDOWN,
        (1, false) => WM_LBUTTONUP,
        (2, true) => WM_RBUTTONDOWN,
        (2, false) => WM_RBUTTONUP,
        (3, true) => WM_MBUTTONDOWN,
        (3, false) => WM_MBUTTONUP,
        (_, true) => WM_XBUTTONDOWN,
        (_, false) => WM_XBUTTONUP,
    };
    let wparam = if button >= 4 {
        ((button - 3) as usize) << 16
    } else {
        0
    };
    let lparam = ((y as u16 as u32) << 16 | x as u16 as u32) as isize;
    unsafe { post_message(window, message, wparam, lparam) };
}

#[cfg(windows)]
pub fn forward_mouse_wheel(window: NativeWindow, delta: i16, horizontal: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{WM_MOUSEHWHEEL, WM_MOUSEWHEEL};
    let message = if horizontal {
        WM_MOUSEHWHEEL
    } else {
        WM_MOUSEWHEEL
    };
    unsafe { post_message(window, message, (delta as u16 as usize) << 16, 0) };
}
