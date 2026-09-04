#[path = "input/dinput.rs"]
pub(crate) mod dinput;
#[path = "input/pointer.rs"]
pub(crate) mod pointer;

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
        RawWindowHandle::Win32(handle) => Ok(NativeWindow(handle.hwnd.get() as isize)),
        _ => Err("Polymer viewer requires a Win32 window".into()),
    }
}
