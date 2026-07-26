//! Hosts the background page on a private desktop.
//!
//! The page has to live in a window, because WebView2 only presents a swapchain
//! when it is hosted in one; visual hosting composes into DirectComposition and
//! presents nothing for the graphics hook to intercept. A window on the user's
//! desktop is a window the user can see, so this puts it on a desktop of its
//! own instead.
//!
//! A desktop belongs to a thread, and `SetThreadDesktop` refuses a thread that
//! already owns windows, so the page gets its own thread with its own message
//! loop. Everything WebView2 spawns from that thread inherits the desktop, so
//! the whole browser process tree lands there too. This is unconditional: the
//! background page is isolated whether or not the guest is.
#![allow(unsafe_code)]

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle,
    Win32WindowHandle, WindowHandle,
};
use std::{
    ffi::c_void,
    num::NonZeroIsize,
    sync::{Arc, Mutex, PoisonError, mpsc},
};
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, PostMessageW,
        RegisterClassW, SW_SHOWNA, ShowWindow, TranslateMessage, WM_APP, WNDCLASSW,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
    },
};
use wry::WebView;

/// Posted to the host thread when scripts are waiting to be evaluated.
const WM_SNOWGLASS_SCRIPT: u32 = WM_APP + 1;

/// The host window, handed to WebView2 as its parent.
pub struct HostWindow(HWND);

impl HasWindowHandle for HostWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let handle = NonZeroIsize::new(self.0.0 as isize).ok_or(HandleError::Unavailable)?;
        // SAFETY: the window outlives this borrow. It belongs to the host
        // thread and is destroyed only when that thread exits.
        unsafe {
            Ok(WindowHandle::borrow_raw(RawWindowHandle::Win32(
                Win32WindowHandle::new(handle),
            )))
        }
    }
}

impl HasDisplayHandle for HostWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Ok(DisplayHandle::windows())
    }
}

pub struct BackgroundHost {
    window: HWND,
    pending: Arc<Mutex<Vec<String>>>,
}

impl BackgroundHost {
    /// Creates the private desktop, its window, and the page inside it, then
    /// leaves the host thread pumping messages for as long as the shell runs.
    ///
    /// `build` runs on the host thread once the desktop is attached, so
    /// everything WebView2 starts from it inherits that desktop.
    pub fn start<B>(width: i32, height: i32, build: B) -> Result<Self, Box<dyn std::error::Error>>
    where
        B: FnOnce(&HostWindow) -> Result<WebView, Box<dyn std::error::Error>> + Send + 'static,
    {
        let name = format!("snowglass-background-{}", std::process::id());
        // WebView2 starts its browser process from a worker thread that is not
        // attached to this desktop, so the process hook has to place it here by
        // name. This must be set before the first WebView2 process exists.
        // SAFETY: set before the host thread, and so before anything reads it.
        unsafe {
            std::env::set_var(snowglass_hook_webview2::DESKTOP, &name);
        }
        let (sender, receiver) = mpsc::channel::<Result<isize, String>>();
        let pending = Arc::new(Mutex::new(Vec::<String>::new()));
        let thread_pending = Arc::clone(&pending);
        std::thread::Builder::new()
            .name("snowglass-background-host".to_owned())
            .spawn(move || {
                let attached =
                    snowglass_platform_windows::PrivateDesktop::create_and_attach_current_thread(
                        &name,
                    );
                let Ok((_desktop, _attachment)) = attached else {
                    let _ = sender.send(Err("could not attach a private desktop".to_owned()));
                    return;
                };
                // WebView2 is apartment-threaded and refuses to build on a
                // thread that has not entered a single-threaded apartment.
                // SAFETY: paired with no uninitialise because this thread lives
                // as long as the process.
                unsafe {
                    let _ = windows::Win32::System::Com::CoInitializeEx(
                        None,
                        windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
                    );
                }
                // SAFETY: the class and window belong to this thread, which
                // pumps their messages until the process exits.
                let Some(window) = (unsafe { create_host_window(width, height) }) else {
                    let _ = sender.send(Err("could not create the background window".to_owned()));
                    return;
                };
                let host = HostWindow(window);
                let webview = match build(&host) {
                    Ok(webview) => webview,
                    Err(error) => {
                        let _ = sender.send(Err(format!("building the background page: {error}")));
                        return;
                    }
                };
                let _ = sender.send(Ok(window.0 as isize));
                // SAFETY: the loop only dispatches to this thread's windows.
                unsafe {
                    let mut message = MSG::default();
                    while GetMessageW(&mut message, None, 0, 0).as_bool() {
                        if message.message == WM_SNOWGLASS_SCRIPT {
                            let scripts = std::mem::take(
                                &mut *thread_pending
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner),
                            );
                            for script in scripts {
                                let _ = webview.evaluate_script(&script);
                            }
                        }
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
            })?;

        match receiver.recv_timeout(std::time::Duration::from_secs(60)) {
            Ok(Ok(window)) => Ok(Self {
                window: HWND(window as *mut c_void),
                pending,
            }),
            Ok(Err(error)) => Err(error.into()),
            Err(_) => Err("the background host thread did not report a window".into()),
        }
    }

    /// Queues a script for the page and wakes the host thread to run it.
    /// WebView2 is apartment-threaded, so it can only be driven from there.
    pub fn evaluate(&self, script: String) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(script);
        // SAFETY: posting to a live window owned by the host thread.
        unsafe {
            let _ = PostMessageW(Some(self.window), WM_SNOWGLASS_SCRIPT, WPARAM(0), LPARAM(0));
        }
    }
}

unsafe fn create_host_window(width: i32, height: i32) -> Option<HWND> {
    unsafe extern "system" fn proc_(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(window, message, w, l) }
    }
    unsafe {
        let class_name = windows::core::w!("SnowglassBackgroundHost");
        let class = WNDCLASSW {
            lpfnWndProc: Some(proc_),
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&class);
        let window = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class_name,
            windows::core::w!("Snowglass Background"),
            WS_POPUP,
            0,
            0,
            width.max(1),
            height.max(1),
            None,
            None,
            None,
            None,
        )
        .ok()?;
        // Shown without activating. On its own desktop nothing competes for
        // focus, and the page has to be visible there to keep painting.
        let _ = ShowWindow(window, SW_SHOWNA);
        Some(window)
    }
}
