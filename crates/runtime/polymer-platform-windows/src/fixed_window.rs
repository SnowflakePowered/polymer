//! Narrow fixed-HWND policy shared through window properties across DLLs.
//! This stores committed policy only; it never selects or assigns a surface.
use crate::{NativeWindow, ProcessId, is_process_window};
use windows::{
    Win32::{
        Foundation::{E_INVALIDARG, HANDLE, HWND, LPARAM, LRESULT, WPARAM},
        UI::WindowsAndMessaging::*,
    },
    core::{BOOL, Error, PCWSTR},
};

const PREFIX: &str = "Polymer.FixedSurface.";

/// Each binding owns a distinct property, so two presentation objects on one
/// HWND cannot remove each other's resize protection. Properties belong to the
/// actual window lifetime, not a process-local copy of a Rust static.
pub struct FixedWindowBinding {
    window: HWND,
    name: Vec<u16>,
    cookie: Box<u8>,
    hook: HHOOK,
}
// This guard never dereferences an HWND/HHOOK or its cookie from the hook
// callback. Win32 property removal and unhooking may run from another thread;
// all Rust-owned storage is immutable until the uniquely owned guard drops.
unsafe impl Send for FixedWindowBinding {}
unsafe impl Sync for FixedWindowBinding {}
impl FixedWindowBinding {
    pub fn new(window: NativeWindow) -> windows::core::Result<Self> {
        if !is_process_window(window, ProcessId(std::process::id())) {
            return Err(Error::new(
                E_INVALIDARG,
                "fixed surface HWND does not belong to the producer",
            ));
        }
        let window = HWND(window.0 as *mut _);
        let thread = unsafe { GetWindowThreadProcessId(window, None) };
        let cookie = Box::new(0u8);
        let name = format!("{PREFIX}{:x}", (&*cookie as *const u8) as usize)
            .encode_utf16()
            .chain([0])
            .collect::<Vec<_>>();
        // A thread-local hook is valid for a window owned by another thread in
        // this same process; no cross-thread SetWindowSubclass call is used.
        let hook =
            unsafe { SetWindowsHookExW(WH_CALLWNDPROC, Some(Self::messages), None, thread)? };
        let binding = Self {
            window,
            name,
            cookie,
            hook,
        };
        unsafe {
            SetPropW(window, PCWSTR(binding.name.as_ptr()), Some(binding.value()))?;
        }
        Ok(binding)
    }
    fn value(&self) -> HANDLE {
        HANDLE((&*self.cookie as *const u8).cast_mut().cast())
    }
    pub fn active(window: NativeWindow) -> bool {
        let mut found = false;
        unsafe {
            EnumPropsExW(
                HWND(window.0 as *mut _),
                Some(Self::find),
                LPARAM((&mut found as *mut bool) as isize),
            );
        }
        found
    }
    unsafe fn owned(name: PCWSTR) -> bool {
        // EnumProps may report an atom instead of a character pointer.
        if name.0 as usize <= u16::MAX as usize {
            return false;
        }
        for (offset, expected) in PREFIX.encode_utf16().enumerate() {
            if unsafe { *name.0.add(offset) } != expected {
                return false;
            }
        }
        true
    }
    unsafe extern "system" fn find(_: HWND, name: PCWSTR, _: HANDLE, context: usize) -> BOOL {
        if unsafe { Self::owned(name) } {
            unsafe {
                *(context as *mut bool) = true;
            }
            BOOL(0)
        } else {
            BOOL(1)
        }
    }
    unsafe extern "system" fn remove(window: HWND, name: PCWSTR, _: HANDLE, _: usize) -> BOOL {
        if unsafe { Self::owned(name) } {
            unsafe {
                let _ = RemovePropW(window, name);
            }
        }
        BOOL(1)
    }
    unsafe extern "system" fn messages(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 {
            let message = unsafe { &*(lparam.0 as *const CWPSTRUCT) };
            if message.message == WM_NCDESTROY {
                // Required before the window procedure finishes NCDESTROY.
                unsafe {
                    EnumPropsExW(message.hwnd, Some(Self::remove), LPARAM(0));
                }
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
}
impl Drop for FixedWindowBinding {
    fn drop(&mut self) {
        unsafe {
            // A destroyed/reused HWND has no property with this live cookie.
            if GetPropW(self.window, PCWSTR(self.name.as_ptr())) == self.value() {
                let _ = RemovePropW(self.window, PCWSTR(self.name.as_ptr()));
            }
            let _ = UnhookWindowsHookEx(self.hook);
        }
    }
}
