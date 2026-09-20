use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFArray, CGPoint};
use objc2_core_graphics::*;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};
use polymer_input::{HostInputEvent, HostPointerInput};
use polymer_platform::{GuestInput, GuestWindowId, ResizeDimensions};
use std::io;

pub struct GuestEvents;
impl GuestEvents {
    pub fn dimensions(pid: u32, window: GuestWindowId) -> io::Result<ResizeDimensions> {
        let bounds = Self::bounds(pid, window)?;
        Ok(ResizeDimensions::new(bounds[2] as u32, bounds[3] as u32))
    }
    fn bounds(pid: u32, window: GuestWindowId) -> io::Result<[f64; 4]> {
        let list =
            CGWindowListCopyWindowInfo(CGWindowListOption::OptionIncludingWindow, window.0 as u32)
                .ok_or_else(|| io::Error::other("window metadata unavailable"))?;
        // SAFETY: CGWindowListCopyWindowInfo returns an array of CFDictionary,
        // toll-free bridged to NSArray<NSDictionary>.
        let list: &NSArray<NSDictionary<NSString, AnyObject>> =
            unsafe { &*((&*list as *const CFArray).cast()) };
        let entry = list
            .firstObject()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let owner = entry
            .objectForKey(&NSString::from_str("kCGWindowOwnerPID"))
            .and_then(|v| v.downcast::<NSNumber>().ok());
        if owner.is_none_or(|v| v.unsignedIntValue() != pid) {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        let bounds = entry
            .objectForKey(&NSString::from_str("kCGWindowBounds"))
            .ok_or_else(|| io::Error::other("window bounds missing"))?;
        let bounds = bounds
            .downcast_ref::<NSDictionary>()
            .ok_or_else(|| io::Error::other("invalid window bounds"))?;
        let mut result = [0.0; 4];
        for (output, key) in result.iter_mut().zip(["X", "Y", "Width", "Height"]) {
            *output = bounds
                .objectForKey(&NSString::from_str(key))
                .and_then(|v| v.downcast::<NSNumber>().ok())
                .ok_or_else(|| io::Error::other("invalid window coordinate"))?
                .doubleValue();
        }
        Ok(result)
    }
    pub fn send(pid: u32, input: GuestInput) -> io::Result<()> {
        if pid != std::process::id() && !CGPreflightPostEventAccess() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Accessibility permission is required to forward guest input",
            ));
        }
        match input {
            GuestInput::Host(HostInputEvent::Key {
                code,
                scan_code,
                pressed,
                ..
            }) => {
                let key = if code.as_str() == "Unidentified" {
                    scan_code
                } else {
                    Self::key_code(code.as_str())?
                };
                let event = CGEvent::new_keyboard_event(None, key, pressed)
                    .ok_or_else(|| io::Error::other("keyboard event allocation failed"))?;
                CGEvent::post_to_pid(pid as i32, Some(&event));
                Ok(())
            }
            GuestInput::Host(HostInputEvent::Pointer(pointer)) => Self::pointer(pid, pointer),
            GuestInput::Host(HostInputEvent::Wheel {
                horizontal,
                vertical,
                ..
            }) => {
                let event = CGEvent::new_scroll_wheel_event2(
                    None,
                    CGScrollEventUnit::Pixel,
                    2,
                    vertical,
                    horizontal,
                    0,
                )
                .ok_or_else(|| io::Error::other("scroll event allocation failed"))?;
                CGEvent::post_to_pid(pid as i32, Some(&event));
                Ok(())
            }
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "unsupported guest input event",
            )),
        }
    }
    fn pointer(pid: u32, pointer: HostPointerInput) -> io::Result<()> {
        if pointer.phase == 4 {
            return Self::send(
                pid,
                GuestInput::Host(HostInputEvent::Wheel {
                    horizontal: pointer.wheel_horizontal,
                    vertical: pointer.wheel_vertical,
                }),
            );
        }
        let bounds = Self::bounds(pid, GuestWindowId(pointer.target_window))?;
        let button = match pointer.button {
            2 => CGMouseButton::Right,
            3.. => CGMouseButton(pointer.button - 1),
            _ => CGMouseButton::Left,
        };
        let kind = match (pointer.phase, button) {
            (2, CGMouseButton::Left) => CGEventType::LeftMouseDown,
            (2, CGMouseButton::Right) => CGEventType::RightMouseDown,
            (2, _) => CGEventType::OtherMouseDown,
            (3 | 5, CGMouseButton::Left) => CGEventType::LeftMouseUp,
            (3 | 5, CGMouseButton::Right) => CGEventType::RightMouseUp,
            (3 | 5, _) => CGEventType::OtherMouseUp,
            (1, _) if pointer.buttons & 1 != 0 => CGEventType::LeftMouseDragged,
            (1, _) if pointer.buttons & 2 != 0 => CGEventType::RightMouseDragged,
            (1, _) => CGEventType::MouseMoved,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid pointer phase",
                ));
            }
        };
        if pointer.coordinate_width == 0 || pointer.coordinate_height == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty pointer coordinate space",
            ));
        }
        let position = CGPoint {
            x: bounds[0] + pointer.x as f64 * bounds[2] / pointer.coordinate_width as f64,
            y: bounds[1] + pointer.y as f64 * bounds[3] / pointer.coordinate_height as f64,
        };
        let event = CGEvent::new_mouse_event(None, kind, position, button)
            .ok_or_else(|| io::Error::other("mouse event allocation failed"))?;
        CGEvent::post_to_pid(pid as i32, Some(&event));
        Ok(())
    }
    pub(crate) fn key_code(code: &str) -> io::Result<u16> {
        const CODES: &[&str] = &[
            "KeyA",
            "KeyS",
            "KeyD",
            "KeyF",
            "KeyH",
            "KeyG",
            "KeyZ",
            "KeyX",
            "KeyC",
            "KeyV",
            "IntlBackslash",
            "KeyB",
            "KeyQ",
            "KeyW",
            "KeyE",
            "KeyR",
            "KeyY",
            "KeyT",
            "Digit1",
            "Digit2",
            "Digit3",
            "Digit4",
            "Digit6",
            "Digit5",
            "Equal",
            "Digit9",
            "Digit7",
            "Minus",
            "Digit8",
            "Digit0",
            "BracketRight",
            "KeyO",
            "KeyU",
            "BracketLeft",
            "KeyI",
            "KeyP",
            "Enter",
            "KeyL",
            "KeyJ",
            "Quote",
            "KeyK",
            "Semicolon",
            "Backslash",
            "Comma",
            "Slash",
            "KeyN",
            "KeyM",
            "Period",
            "Tab",
            "Space",
            "Backquote",
            "Backspace",
        ];
        if let Some(index) = CODES.iter().position(|candidate| *candidate == code) {
            return Ok(index as u16);
        }
        // HIToolbox Events.h virtual key codes; function keys are not contiguous.
        match code {
            "F1" => Ok(0x7A),
            "F2" => Ok(0x78),
            "F3" => Ok(0x63),
            "F4" => Ok(0x76),
            "F5" => Ok(0x60),
            "F6" => Ok(0x61),
            "F7" => Ok(0x62),
            "F8" => Ok(0x64),
            "F9" => Ok(0x65),
            "F10" => Ok(0x6D),
            "F11" => Ok(0x67),
            "F12" => Ok(0x6F),
            "F13" => Ok(0x69),
            "F14" => Ok(0x6B),
            "F15" => Ok(0x71),
            "F16" => Ok(0x6A),
            "F17" => Ok(0x40),
            "F18" => Ok(0x4F),
            "F19" => Ok(0x50),
            "F20" => Ok(0x5A),
            "Escape" => Ok(53),
            "MetaRight" => Ok(54),
            "MetaLeft" => Ok(55),
            "ShiftLeft" => Ok(56),
            "CapsLock" => Ok(57),
            "AltLeft" => Ok(58),
            "ControlLeft" => Ok(59),
            "ShiftRight" => Ok(60),
            "AltRight" => Ok(61),
            "ControlRight" => Ok(62),
            "ArrowLeft" => Ok(123),
            "ArrowRight" => Ok(124),
            "ArrowDown" => Ok(125),
            "ArrowUp" => Ok(126),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("unmapped macOS key {code}"),
            )),
        }
    }
}
