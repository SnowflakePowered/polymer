use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSEvent, NSEventType, NSScreen};
use objc2_core_foundation::CFData;
use objc2_core_graphics::{CGEvent, CGEventField};
use objc2_foundation::NSPoint;
use std::io;

pub(super) struct AutomationInput;
impl AutomationInput {
    pub fn dispatch(bytes: &[u8]) -> io::Result<()> {
        let main = MainThreadMarker::new()
            .ok_or_else(|| io::Error::other("AppKit input requires the main thread"))?;
        let (pid, bytes) = bytes
            .split_at_checked(4)
            .ok_or_else(|| io::Error::other("native event is missing its process ID"))?;
        let pid = u32::from_le_bytes(pid.try_into().unwrap());
        if pid != std::process::id() {
            return Ok(());
        }
        let data = CFData::from_bytes(bytes);
        let cg = CGEvent::from_data(None, Some(&data))
            .ok_or_else(|| io::Error::other("invalid native event"))?;
        let app = NSApplication::sharedApplication(main);
        let target = CGEvent::integer_value_field(
            Some(&cg),
            CGEventField::MouseEventWindowUnderMousePointer,
        ) as isize;
        let window = app
            .windows()
            .into_iter()
            .find(|w| w.windowNumber() == target)
            .or_else(|| (target == 0).then(|| app.keyWindow()).flatten())
            .ok_or_else(|| io::Error::other("automation window is unavailable"))?;
        let event = NSEvent::eventWithCGEvent(&cg)
            .ok_or_else(|| io::Error::other("could not create AppKit event"))?;
        let event = match event.r#type() {
            NSEventType::LeftMouseDown
            | NSEventType::LeftMouseUp
            | NSEventType::RightMouseDown
            | NSEventType::RightMouseUp
            | NSEventType::OtherMouseDown
            | NSEventType::OtherMouseUp
            | NSEventType::MouseMoved => {
                let position = CGEvent::location(Some(&cg));
                let screen = NSScreen::screens(main)
                    .firstObject()
                    .ok_or_else(|| io::Error::other("screen is unavailable"))?;
                let location = window.convertPointFromScreen(NSPoint::new(
                    position.x,
                    screen.frame().size.height - position.y,
                ));
                NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(event.r#type(), location, event.modifierFlags(), event.timestamp(), window.windowNumber(), None, 0, CGEvent::integer_value_field(Some(&cg), CGEventField::MouseEventClickState) as isize, 1.0)
                    .ok_or_else(|| io::Error::other("could not create window mouse event"))?
            }
            _ => event,
        };
        // Deliver directly to the isolated window; WindowServer hit testing is
        // tied to the user's active Space and would discard these events.
        window.sendEvent(&event);
        Ok(())
    }
}
