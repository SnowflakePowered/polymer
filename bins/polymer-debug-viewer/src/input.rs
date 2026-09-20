#[path = "input/dinput.rs"]
pub(crate) mod dinput;
#[path = "input/pointer.rs"]
pub(crate) mod pointer;

#[cfg(target_os = "macos")]
use polymer_platform_macos::NativeWindow;
#[cfg(windows)]
use polymer_platform_windows::NativeWindow;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    error::Error,
    sync::atomic::{AtomicIsize, Ordering},
};
use winit::window::Window;

pub(crate) fn load_window(window: &AtomicIsize) -> Option<NativeWindow> {
    let value = window.load(Ordering::Acquire);
    (value != 0).then_some(NativeWindow(value))
}

pub(crate) fn native_window(window: &Window) -> Result<NativeWindow, Box<dyn Error>> {
    match window.window_handle()?.as_raw() {
        #[cfg(windows)]
        RawWindowHandle::Win32(handle) => Ok(NativeWindow(handle.hwnd.get() as isize)),
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(handle) => Ok(NativeWindow(handle.ns_view.as_ptr() as isize)),
        _ => Err("unsupported native viewer window".into()),
    }
}
