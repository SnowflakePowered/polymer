use dispatch2::DispatchQueue;
use polymer_input::{HostInputEvent, HostPointerInput, KeyCode};
use polymer_ipc::{
    Message,
    input::{InputMessage, InputSubscriber},
};
use polymer_platform::GuestInput;
use polymer_platform_macos::GuestEvents;
use std::{io, time::Duration};

pub(super) struct Input;
impl Input {
    pub fn initialize() {
        let Ok(session) = std::env::var("POLYMER_SESSION") else {
            return;
        };
        super::keyboard::Keyboard::attach();
        let _ = std::thread::Builder::new()
            .name("polymer-appkit-input".into())
            .spawn(move || {
                if let Err(error) = Self::receive(&session) {
                    eprintln!("Polymer AppKit input ended: {error}");
                }
                super::keyboard::Keyboard::clear();
            });
    }
    fn receive(session: &str) -> io::Result<()> {
        let mut subscriber = InputSubscriber::connect(&format!("{session}-input"))?;
        loop {
            while let Some(message) = subscriber.control.try_receive()? {
                match message {
                    Message::Blob {
                        kind: polymer_ipc::BlobKind::MacosAutomationEvent,
                        data: bytes,
                    } => {
                        DispatchQueue::main().exec_async(move || {
                            if let Err(error) = super::automation::AutomationInput::dispatch(&bytes)
                            {
                                eprintln!("Polymer automation input failed: {error}");
                            }
                        });
                    }
                    Message::Shutdown => return Ok(()),
                    _ => {}
                }
            }
            subscriber.drain(|message| {
                // Action keys use native macOS key codes and must stay held
                // across polling ticks, even while the UI thread is busy.
                if let InputMessage::HostKey(key) = &message {
                    if KeyCode::from_wire(key.code).as_str() == "Unidentified" {
                        super::keyboard::Keyboard::set(key.scan_code, key.pressed);
                        return;
                    }
                }
                let event = match message {
                    InputMessage::HostKey(key) => HostInputEvent::Key {
                        code: KeyCode::from_wire(key.code),
                        scan_code: key.scan_code,
                        virtual_key: key.virtual_key,
                        pressed: key.pressed,
                        repeat: key.repeat,
                        extended: key.extended,
                        target_window: key.target_window,
                        timestamp_ms: key.timestamp_ms,
                        sequence: key.sequence,
                    },
                    InputMessage::HostPointer(p) => HostInputEvent::Pointer(HostPointerInput {
                        surface_id: p.surface_id,
                        target_window: p.target_window,
                        source: p.source,
                        phase: p.phase,
                        pointer_id: p.pointer_id,
                        x: p.x,
                        y: p.y,
                        coordinate_width: p.coordinate_width,
                        coordinate_height: p.coordinate_height,
                        button: p.button,
                        position: p.position.map(f64::from),
                        delta: p.delta.map(f64::from),
                        buttons: p.buttons,
                        wheel_horizontal: p.wheel_horizontal,
                        wheel_vertical: p.wheel_vertical,
                        timestamp_ms: p.timestamp_ms,
                    }),
                    InputMessage::HostWheel(wheel) => HostInputEvent::Wheel {
                        horizontal: wheel.horizontal,
                        vertical: wheel.vertical,
                    },
                    _ => return,
                };
                DispatchQueue::main().exec_async(move || {
                    if let Err(error) =
                        GuestEvents::send(std::process::id(), GuestInput::Host(event))
                    {
                        eprintln!("Polymer AppKit input failed: {error}");
                    }
                });
            })?;
            std::thread::sleep(Duration::from_millis(4));
        }
    }
}
