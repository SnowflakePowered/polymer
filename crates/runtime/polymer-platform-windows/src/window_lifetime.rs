use crate::{NativeWindow, ProcessId, is_process_window};
use std::sync::{
    LazyLock, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use windows::{
    Win32::{Foundation::*, UI::WindowsAndMessaging::*},
    core::{Error, PCWSTR},
};

static NEXT: AtomicUsize = AtomicUsize::new(1);
// Each linked producer owns its identity domain; separate property names avoid
// racing another DLL's copy of these statics.
static NAME: LazyLock<Vec<u16>> = LazyLock::new(|| {
    format!(
        "Polymer.WindowLifetime.{:x}",
        &NEXT as *const AtomicUsize as usize
    )
    .encode_utf16()
    .chain([0])
    .collect()
});
// This is an ordered list of live windows, not a shared hash map. A window
// owns its hook until NCDESTROY even if all presentation objects disappear.
static HOOKS: Mutex<Vec<(usize, usize, usize)>> = Mutex::new(Vec::new());

pub struct WindowLifetime;
impl WindowLifetime {
    pub fn identify(window: NativeWindow) -> windows::core::Result<u64> {
        if !is_process_window(window, ProcessId(std::process::id())) {
            return Err(Error::new(
                E_INVALIDARG,
                "presentation window belongs to another process",
            ));
        }
        let window = HWND(window.0 as *mut _);
        // These property/hook-management calls do not invoke the application
        // window procedure. Serialize them with destruction bookkeeping.
        let mut hooks = HOOKS.lock().map_err(|_| Error::from_hresult(E_FAIL))?;
        let existing = unsafe { GetPropW(window, PCWSTR(NAME.as_ptr())) };
        if !existing.is_invalid() {
            return Ok(existing.0 as u64);
        }
        let serial = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| Error::new(E_FAIL, "window lifetime counter exhausted"))?;
        let thread = unsafe { GetWindowThreadProcessId(window, None) };
        let hook = unsafe { SetWindowsHookExW(WH_CALLWNDPROC, Some(Self::message), None, thread)? };
        if let Err(error) = unsafe {
            SetPropW(
                window,
                PCWSTR(NAME.as_ptr()),
                Some(HANDLE(serial as *mut _)),
            )
        } {
            unsafe {
                let _ = UnhookWindowsHookEx(hook);
            }
            return Err(error);
        }
        hooks.push((window.0 as usize, serial, hook.0 as usize));
        Ok(serial as u64)
    }

    unsafe extern "system" fn message(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 {
            let message = unsafe { &*(lparam.0 as *const CWPSTRUCT) };
            if message.message == WM_NCDESTROY {
                let hook = HOOKS.lock().ok().and_then(|mut hooks| {
                    let serial =
                        unsafe { GetPropW(message.hwnd, PCWSTR(NAME.as_ptr())) }.0 as usize;
                    let index = hooks.iter().position(|(window, owned, _)| {
                        *window == message.hwnd.0 as usize && *owned == serial
                    })?;
                    unsafe {
                        let _ = RemovePropW(message.hwnd, PCWSTR(NAME.as_ptr()));
                    }
                    Some(hooks.swap_remove(index).2)
                });
                if let Some(hook) = hook {
                    unsafe {
                        let _ = UnhookWindowsHookEx(HHOOK(hook as *mut _));
                    }
                }
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
}
