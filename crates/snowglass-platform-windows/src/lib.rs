//! Windows-specific handle, adapter, process, and window utilities.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct ProcessId(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub struct NativeWindow(pub isize);

#[cfg(windows)]
pub struct WindowKeyForwarder {
    viewer: windows_sys::Win32::Foundation::HWND,
    context: *mut std::sync::atomic::AtomicIsize,
}

#[cfg(windows)]
impl WindowKeyForwarder {
    pub fn install(
        viewer: NativeWindow,
        backing_window: std::sync::Arc<std::sync::atomic::AtomicIsize>,
    ) -> std::io::Result<Self> {
        use windows_sys::Win32::UI::Shell::SetWindowSubclass;
        let context =
            std::sync::Arc::into_raw(backing_window) as *mut std::sync::atomic::AtomicIsize;
        let viewer = viewer.0 as windows_sys::Win32::Foundation::HWND;
        // SAFETY: context remains allocated until Drop removes this exact
        // subclass, and the viewer HWND is live while the forwarder is held.
        if unsafe {
            SetWindowSubclass(
                viewer,
                Some(key_forward_subclass),
                context as usize,
                context as usize,
            )
        } == 0
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
        use windows_sys::Win32::UI::Shell::RemoveWindowSubclass;
        // SAFETY: this removes the callback before releasing its context.
        unsafe {
            RemoveWindowSubclass(
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
    window: windows_sys::Win32::Foundation::HWND,
    message: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
    _subclass_id: usize,
    context: usize,
) -> windows_sys::Win32::Foundation::LRESULT {
    use std::sync::atomic::Ordering;
    use windows_sys::Win32::UI::{
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
    if message == WM_SIZE && wparam != SIZE_MINIMIZED as usize && target != 0 {
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
            unsafe { PostMessageW(target as _, message, wparam, lparam) };
        }
    }
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

#[cfg(windows)]
unsafe fn post_logical_focus(target: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        PostMessageW, WA_ACTIVE, WM_ACTIVATE, WM_ACTIVATEAPP, WM_SETFOCUS,
    };
    unsafe {
        PostMessageW(target as _, WM_ACTIVATEAPP, 1, 0);
        PostMessageW(target as _, WM_ACTIVATE, WA_ACTIVE as usize, 0);
        PostMessageW(target as _, WM_SETFOCUS, 0, 0);
    }
}

#[cfg(windows)]
pub fn forward_focus(window: NativeWindow, focused: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        PostMessageW, WA_INACTIVE, WM_ACTIVATE, WM_ACTIVATEAPP, WM_KILLFOCUS,
    };
    unsafe {
        if focused {
            post_logical_focus(window.0);
        } else {
            PostMessageW(window.0 as _, WM_KILLFOCUS, 0, 0);
            PostMessageW(window.0 as _, WM_ACTIVATE, WA_INACTIVE as usize, 0);
            PostMessageW(window.0 as _, WM_ACTIVATEAPP, 0, 0);
        }
    }
}

#[cfg(windows)]
pub fn performance_counter() -> std::io::Result<u64> {
    let mut value = 0i64;
    // SAFETY: the pointer refers to writable storage for the duration of the call.
    if unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut value) } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(value as u64)
    }
}

#[cfg(windows)]
pub fn performance_frequency() -> std::io::Result<u64> {
    let mut value = 0i64;
    // SAFETY: the pointer refers to writable storage for the duration of the call.
    if unsafe { windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut value) }
        == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(value as u64)
    }
}

#[cfg(windows)]
pub fn duplicate_handle_into_process(
    source_handle: isize,
    target_process_id: ProcessId,
) -> std::io::Result<isize> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE},
        System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE},
    };

    // SAFETY: handles are checked for null, the target process handle is closed
    // on every path, and DuplicateHandle initializes `duplicated` on success.
    unsafe {
        let target_process = OpenProcess(PROCESS_DUP_HANDLE, 0, target_process_id.0);
        if target_process.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut duplicated: HANDLE = std::ptr::null_mut();
        let result = DuplicateHandle(
            GetCurrentProcess(),
            source_handle as HANDLE,
            target_process,
            &mut duplicated,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        );
        let error = (result == 0).then(std::io::Error::last_os_error);
        CloseHandle(target_process);
        if let Some(error) = error {
            Err(error)
        } else {
            Ok(duplicated as isize)
        }
    }
}

#[cfg(windows)]
pub fn hide_top_level_windows(process_id: ProcessId) -> std::io::Result<u32> {
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM},
        UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, SW_HIDE, ShowWindow},
    };

    struct Context {
        process_id: u32,
        hidden: u32,
    }

    unsafe extern "system" fn callback(window: HWND, parameter: LPARAM) -> i32 {
        // SAFETY: EnumWindows supplies the pointer passed below for the complete
        // duration of enumeration.
        let context = unsafe { &mut *(parameter as *mut Context) };
        let mut owner = 0;
        unsafe { GetWindowThreadProcessId(window, &mut owner) };
        if owner == context.process_id {
            unsafe { ShowWindow(window, SW_HIDE) };
            context.hidden += 1;
        }
        1
    }

    let mut context = Context {
        process_id: process_id.0,
        hidden: 0,
    };
    // SAFETY: callback and context remain valid until EnumWindows returns.
    if unsafe { EnumWindows(Some(callback), (&mut context as *mut Context) as LPARAM) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(context.hidden)
}

#[cfg(windows)]
pub fn find_main_window(process_id: ProcessId) -> Option<NativeWindow> {
    use windows_sys::Win32::{
        Foundation::{HWND, LPARAM, RECT},
        UI::WindowsAndMessaging::{EnumWindows, GetClientRect, GetWindowThreadProcessId},
    };

    struct Context {
        process_id: u32,
        best: HWND,
        score: i64,
    }
    unsafe extern "system" fn callback(window: HWND, parameter: LPARAM) -> i32 {
        let context = unsafe { &mut *(parameter as *mut Context) };
        let mut owner = 0;
        unsafe { GetWindowThreadProcessId(window, &mut owner) };
        if owner == context.process_id {
            let mut rect = RECT::default();
            if unsafe { GetClientRect(window, &mut rect) } != 0 {
                let area = i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top);
                let style = unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongW(
                        window,
                        windows_sys::Win32::UI::WindowsAndMessaging::GWL_STYLE,
                    )
                } as u32;
                let borderless =
                    style & windows_sys::Win32::UI::WindowsAndMessaging::WS_CAPTION == 0;
                let score = area + if borderless { 1i64 << 60 } else { 0 };
                if score > context.score {
                    context.best = window;
                    context.score = score;
                }
            }
        }
        1
    }
    let mut context = Context {
        process_id: process_id.0,
        best: std::ptr::null_mut(),
        score: 0,
    };
    unsafe { EnumWindows(Some(callback), (&mut context as *mut Context) as LPARAM) };
    (!context.best.is_null()).then_some(NativeWindow(context.best as isize))
}

#[cfg(windows)]
pub fn is_borderless_window(window: NativeWindow) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GWL_STYLE, GetWindowLongW, WS_CAPTION};
    let style = unsafe { GetWindowLongW(window.0 as _, GWL_STYLE) } as u32;
    style & WS_CAPTION == 0
}

#[cfg(windows)]
pub fn client_size(window: NativeWindow) -> Option<(u32, u32)> {
    let mut rect = windows_sys::Win32::Foundation::RECT::default();
    (unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(window.0 as _, &mut rect)
    } != 0)
        .then_some((
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        ))
}

#[cfg(windows)]
pub fn is_process_window(window: NativeWindow, process_id: ProcessId) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow};
    let hwnd = window.0 as windows_sys::Win32::Foundation::HWND;
    if unsafe { IsWindow(hwnd) } == 0 {
        return false;
    }
    let mut owner = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
    owner == process_id.0
}

#[cfg(windows)]
pub fn resize_client(window: NativeWindow, width: u32, height: u32) -> std::io::Result<()> {
    use windows_sys::Win32::{
        Foundation::{HWND, RECT},
        UI::WindowsAndMessaging::{
            AdjustWindowRectEx, GWL_EXSTYLE, GWL_STYLE, GetWindowLongW, SWP_HIDEWINDOW, SWP_NOMOVE,
            SWP_NOZORDER, SetWindowPos,
        },
    };
    if width == 0 || height == 0 {
        return Ok(());
    }
    let hwnd = window.0 as HWND;
    let mut client = RECT::default();
    if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client) } != 0
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
    if unsafe { AdjustWindowRectEx(&mut rect, style, 0, ex_style) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOMOVE | SWP_NOZORDER | SWP_HIDEWINDOW,
        )
    } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
pub fn forward_key(window: NativeWindow, virtual_key: u32, pressed: bool, repeat: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_KEYDOWN, WM_KEYUP};
    let message = if pressed { WM_KEYDOWN } else { WM_KEYUP };
    let mut lparam = 1isize;
    if repeat || !pressed {
        lparam |= 1 << 30;
    }
    if !pressed {
        lparam |= 1 << 31;
    }
    unsafe { PostMessageW(window.0 as _, message, virtual_key as usize, lparam) };
}

#[cfg(windows)]
pub fn forward_character(window: NativeWindow, character: char) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CHAR};
    let mut units = [0; 2];
    for unit in character.encode_utf16(&mut units) {
        unsafe { PostMessageW(window.0 as _, WM_CHAR, *unit as usize, 1) };
    }
}

#[cfg(windows)]
pub fn forward_mouse_move(window: NativeWindow, x: i32, y: i32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_MOUSEMOVE};
    let lparam = ((y as u16 as u32) << 16 | x as u16 as u32) as isize;
    unsafe { PostMessageW(window.0 as _, WM_MOUSEMOVE, 0, lparam) };
}

#[cfg(windows)]
pub fn forward_mouse_button(window: NativeWindow, button: u16, pressed: bool, x: i32, y: i32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        PostMessageW, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN,
        WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
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
    unsafe { PostMessageW(window.0 as _, message, wparam, lparam) };
}

#[cfg(windows)]
pub fn forward_mouse_wheel(window: NativeWindow, delta: i16, horizontal: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        PostMessageW, WM_MOUSEHWHEEL, WM_MOUSEWHEEL,
    };
    let message = if horizontal {
        WM_MOUSEHWHEEL
    } else {
        WM_MOUSEWHEEL
    };
    unsafe { PostMessageW(window.0 as _, message, (delta as u16 as usize) << 16, 0) };
}
