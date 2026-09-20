use super::MacAutomationDriver;
use crate::GuestEvents;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFArray, CGPoint};
use objc2_core_graphics::*;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};
use polymer_automation::{AutomationError, InputCommand};
use xa11y_core::{ElementData, Provider};

pub(super) struct TargetedInput<'a> {
    input: &'a polymer_ipc::input::host::InputIpcListener,
    window: u32,
}
impl TargetedInput<'_> {
    pub fn execute(
        input: &polymer_ipc::input::host::InputIpcListener,
        pid: u32,
        provider: &dyn Provider,
        root: &ElementData,
        command: InputCommand,
    ) -> Result<(), AutomationError> {
        if matches!(command, InputCommand::LogicalFocus) {
            return provider.focus(root).map_err(MacAutomationDriver::map_error);
        }
        let sender = TargetedInput {
            input,
            window: Self::window(pid, root)?,
        };
        match command {
            InputCommand::LogicalFocus => {
                provider.focus(root).map_err(MacAutomationDriver::map_error)
            }
            InputCommand::KeyDown { code } => {
                sender.key(pid, &code, true, false, CGEventFlags::empty())
            }
            InputCommand::KeyUp { code } => {
                sender.key(pid, &code, false, false, CGEventFlags::empty())
            }
            InputCommand::KeyPress {
                code,
                modifiers,
                repeat,
            } => {
                let mut flags = CGEventFlags::empty();
                for modifier in modifiers {
                    flags |= match modifier.as_str() {
                        "ShiftLeft" | "ShiftRight" => CGEventFlags::MaskShift,
                        "ControlLeft" | "ControlRight" => CGEventFlags::MaskControl,
                        "AltLeft" | "AltRight" => CGEventFlags::MaskAlternate,
                        "MetaLeft" | "MetaRight" => CGEventFlags::MaskCommand,
                        _ => {
                            return Err(AutomationError::invalid_argument(
                                "unknown keyboard modifier",
                            ));
                        }
                    };
                }
                sender.key(pid, &code, true, repeat, flags)?;
                sender.key(pid, &code, false, false, flags)
            }
            InputCommand::TypeText { text } => {
                for character in text.chars() {
                    let mut buffer = [0; 2];
                    let utf16 = character.encode_utf16(&mut buffer);
                    for down in [true, false] {
                        let event =
                            CGEvent::new_keyboard_event(None, 0, down).ok_or_else(|| {
                                AutomationError::platform("could not allocate keyboard event")
                            })?;
                        // SAFETY: the UTF-16 slice remains live for this call.
                        unsafe {
                            CGEvent::keyboard_set_unicode_string(
                                Some(&event),
                                utf16.len() as u64,
                                utf16.as_ptr(),
                            );
                        }
                        sender.send(pid, &event)?;
                    }
                }
                Ok(())
            }
            InputCommand::MouseMove { x, y } => sender.mouse(pid, "left", x, y, None, 0),
            InputCommand::MouseDown { button, x, y } => {
                sender.mouse(pid, &button, x, y, Some(true), 1)
            }
            InputCommand::MouseUp { button, x, y } => {
                sender.mouse(pid, &button, x, y, Some(false), 1)
            }
            InputCommand::MouseClick {
                button,
                count,
                x,
                y,
            } => {
                if !(1..=3).contains(&count) {
                    return Err(AutomationError::invalid_argument(
                        "click count must be between 1 and 3",
                    ));
                }
                for click in 1..=count {
                    sender.mouse(pid, &button, x, y, Some(true), click)?;
                    sender.mouse(pid, &button, x, y, Some(false), click)?;
                }
                Ok(())
            }
            InputCommand::MouseWheel { delta_x, delta_y } => {
                let event = CGEvent::new_scroll_wheel_event2(
                    None,
                    CGScrollEventUnit::Pixel,
                    2,
                    delta_y,
                    delta_x,
                    0,
                )
                .ok_or_else(|| AutomationError::platform("could not allocate scroll event"))?;
                sender.send(pid, &event)?;
                Ok(())
            }
        }
    }

    fn key(
        &self,
        pid: u32,
        code: &str,
        down: bool,
        repeat: bool,
        flags: CGEventFlags,
    ) -> Result<(), AutomationError> {
        let key = GuestEvents::key_code(code)
            .map_err(|e| AutomationError::invalid_argument(e.to_string()))?;
        let event = CGEvent::new_keyboard_event(None, key, down)
            .ok_or_else(|| AutomationError::platform("could not allocate keyboard event"))?;
        CGEvent::set_flags(Some(&event), flags);
        CGEvent::set_integer_value_field(
            Some(&event),
            CGEventField::KeyboardEventAutorepeat,
            i64::from(repeat),
        );
        self.send(pid, &event)?;
        Ok(())
    }

    fn mouse(
        &self,
        pid: u32,
        button: &str,
        x: i32,
        y: i32,
        down: Option<bool>,
        count: u32,
    ) -> Result<(), AutomationError> {
        let (button, press, release) = match button {
            "left" => (
                CGMouseButton::Left,
                CGEventType::LeftMouseDown,
                CGEventType::LeftMouseUp,
            ),
            "right" => (
                CGMouseButton::Right,
                CGEventType::RightMouseDown,
                CGEventType::RightMouseUp,
            ),
            "middle" => (
                CGMouseButton::Center,
                CGEventType::OtherMouseDown,
                CGEventType::OtherMouseUp,
            ),
            _ => return Err(AutomationError::invalid_argument("unknown mouse button")),
        };
        let kind = match down {
            Some(true) => press,
            Some(false) => release,
            None => CGEventType::MouseMoved,
        };
        let event = CGEvent::new_mouse_event(None, kind, CGPoint::new(x.into(), y.into()), button)
            .ok_or_else(|| AutomationError::platform("could not allocate mouse event"))?;
        CGEvent::set_integer_value_field(
            Some(&event),
            CGEventField::MouseEventClickState,
            count.into(),
        );
        self.send(pid, &event)?;
        Ok(())
    }
    pub(super) fn window(pid: u32, root: &ElementData) -> Result<u32, AutomationError> {
        let windows = CGWindowListCopyWindowInfo(CGWindowListOption::OptionAll, 0)
            .ok_or_else(|| AutomationError::platform("window metadata unavailable"))?;
        // SAFETY: CGWindowListCopyWindowInfo returns toll-free bridged dictionaries.
        let windows: &NSArray<NSDictionary<NSString, AnyObject>> =
            unsafe { &*((&*windows as *const CFArray).cast()) };
        let bounds = root
            .bounds
            .ok_or_else(|| AutomationError::new("notInteractable", "window has no bounds"))?;
        let expected = [
            bounds.x as f64,
            bounds.y as f64,
            bounds.width as f64,
            bounds.height as f64,
        ];
        for entry in windows {
            let number = |key: &str| {
                entry
                    .objectForKey(&NSString::from_str(key))
                    .and_then(|v| v.downcast::<NSNumber>().ok())
                    .map(|n| n.unsignedIntValue())
            };
            if number("kCGWindowOwnerPID") != Some(pid) {
                continue;
            }
            let Some(bounds) = entry.objectForKey(&NSString::from_str("kCGWindowBounds")) else {
                continue;
            };
            let Some(bounds) = bounds.downcast_ref::<NSDictionary>() else {
                continue;
            };
            let mut rect = [0.0; 4];
            for (value, key) in rect.iter_mut().zip(["X", "Y", "Width", "Height"]) {
                *value = bounds
                    .objectForKey(&NSString::from_str(key))
                    .and_then(|v| v.downcast::<NSNumber>().ok())
                    .map(|n| n.doubleValue())
                    .unwrap_or_default();
            }
            if rect
                .iter()
                .zip(expected)
                .any(|(actual, expected)| (actual - expected).abs() > 2.0)
            {
                continue;
            }
            if let Some(window) = number("kCGWindowNumber") {
                return Ok(window);
            }
        }
        Err(AutomationError::new(
            "staleWindow",
            "native window no longer matches accessibility bounds",
        ))
    }
    fn send(&self, pid: u32, event: &CGEvent) -> Result<(), AutomationError> {
        for field in [
            CGEventField::MouseEventWindowUnderMousePointer,
            CGEventField::MouseEventWindowUnderMousePointerThatCanHandleThisEvent,
        ] {
            CGEvent::set_integer_value_field(Some(event), field, self.window.into());
        }
        let bytes = CGEvent::new_data(None, Some(event))
            .ok_or_else(|| AutomationError::platform("could not serialize native event"))?;
        // CGEvent serialization deliberately omits the destination PID.
        let mut payload = pid.to_le_bytes().to_vec();
        payload.extend_from_slice(&bytes.to_vec());
        if !self.input.send_control(polymer_ipc::Message::Blob {
            kind: polymer_ipc::BlobKind::MacosAutomationEvent,
            data: payload,
        }) {
            return Err(AutomationError::new(
                "processExited",
                "guest input connection is closed",
            ));
        }
        Ok(())
    }
}
