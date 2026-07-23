#[path = "input/dinput.rs"]
pub(crate) mod dinput;
#[path = "input/pointer.rs"]
pub(crate) mod pointer;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use snowglass_platform_windows::NativeWindow;
use std::{
    error::Error,
    sync::atomic::{AtomicIsize, Ordering},
};
use winit::window::Window;

pub(crate) fn pointer_axes(x: f64, y: f64) -> Option<[i16; 2]> {
    fn axis(value: f64) -> Option<i16> {
        if !value.is_finite() {
            return None;
        }
        let value = value.clamp(0.0, 1.0);
        let scaled = if value <= 0.5 {
            value * 65_536.0 - 32_768.0
        } else {
            (value - 0.5) * 65_534.0
        }
        .round();
        Some(scaled.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16)
    }

    Some([axis(x)?, axis(y)?])
}

pub(crate) fn load_window(window: &AtomicIsize) -> Option<NativeWindow> {
    let value = window.load(Ordering::Acquire);
    (value != 0).then_some(NativeWindow(value))
}

pub(crate) fn native_window(window: &Window) -> Result<NativeWindow, Box<dyn Error>> {
    match window.window_handle()?.as_raw() {
        RawWindowHandle::Win32(handle) => Ok(NativeWindow(handle.hwnd.get() as isize)),
        _ => Err("Snowglass viewer requires a Win32 window".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::pointer_axes;

    #[test]
    fn absolute_pointer_fills_signed_axis_range() {
        assert_eq!(pointer_axes(0.0, 0.0), Some([i16::MIN, i16::MIN]));
        assert_eq!(pointer_axes(0.5, 0.5), Some([0, 0]));
        assert_eq!(pointer_axes(1.0, 1.0), Some([i16::MAX, i16::MAX]));
    }

    #[test]
    fn invalid_pointer_position_is_not_published() {
        assert_eq!(pointer_axes(f64::NAN, 0.0), None);
        assert_eq!(pointer_axes(0.0, f64::INFINITY), None);
    }
}
