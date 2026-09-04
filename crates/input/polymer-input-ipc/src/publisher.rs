use polymer_input::{DeviceDescriptor, DeviceState, HostInputEvent, InputEvent, PlayerIndex};
use polymer_ipc::input::{
    InputMessage, InputPublisher, WireDeviceConnected, WireDeviceState, WireHostKey,
    WireHostPointer, WireHostWheel,
};
use std::io;

pub struct InputPublication {
    publisher: InputPublisher,
    sequences: [u32; 8],
}

impl InputPublication {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            publisher: InputPublisher::new()?,
            sequences: [0; 8],
        })
    }

    pub fn accept(&mut self, socket: &mut polymer_ipc::LocalSocket) -> io::Result<u32> {
        self.publisher.accept(socket)
    }

    pub fn on_input(&mut self, event: InputEvent<'_>) {
        let message = match event {
            InputEvent::Connected {
                index: Some(index),
                device,
            } => Some(InputMessage::DeviceConnected(WireDeviceConnected {
                index: index.get(),
                capabilities: device.capabilities.bits(),
            })),
            InputEvent::Disconnected {
                index: Some(index), ..
            } => Some(InputMessage::DeviceDisconnected { index: index.get() }),
            InputEvent::State {
                index,
                sequence,
                state,
                ..
            } => {
                self.sequences[usize::from(index.get())] = sequence;
                Some(InputMessage::DeviceState(WireDeviceState {
                    index: index.get(),
                    sequence,
                    buttons: state.gamepad.buttons,
                    left_stick: state.gamepad.left_stick,
                    right_stick: state.gamepad.right_stick,
                    left_trigger: state.gamepad.left_trigger,
                    right_trigger: state.gamepad.right_trigger,
                    pointer_position: state.pointer.position,
                    pointer_delta: state.pointer.delta,
                    pointer_flags: state.pointer.flags.bits(),
                    pointer_kind: state.pointer.kind as u8,
                    accelerometer: state.sensor.accelerometer,
                    gyroscope: state.sensor.gyroscope,
                    orientation: state.sensor.orientation,
                    has_orientation: state.sensor.has_orientation,
                    sampled_at_ns: state.sensor.sampled_at_ns,
                    battery_level: state.battery.level,
                    charging: state.battery.charging,
                }))
            }
            _ => None,
        };
        if let Some(message) = message {
            let _ = self.publisher.publish(message);
        }
    }

    pub fn connect(&mut self, index: PlayerIndex, device: &DeviceDescriptor) {
        let _ = self
            .publisher
            .publish(InputMessage::DeviceConnected(WireDeviceConnected {
                index: index.get(),
                capabilities: device.capabilities.bits(),
            }));
    }

    pub fn disconnect(&mut self, index: PlayerIndex) {
        let _ = self
            .publisher
            .publish(InputMessage::DeviceDisconnected { index: index.get() });
    }

    pub fn publish_action(&mut self, index: PlayerIndex, buttons: u32) {
        let sequence = &mut self.sequences[usize::from(index.get())];
        *sequence = sequence.wrapping_add(1);
        let state = DeviceState {
            gamepad: polymer_input::GamepadReport {
                buttons,
                ..Default::default()
            },
            ..Default::default()
        };
        let _ = self
            .publisher
            .publish(InputMessage::DeviceState(WireDeviceState {
                index: index.get(),
                sequence: *sequence,
                buttons: state.gamepad.buttons,
                left_stick: state.gamepad.left_stick,
                right_stick: state.gamepad.right_stick,
                left_trigger: state.gamepad.left_trigger,
                right_trigger: state.gamepad.right_trigger,
                pointer_position: state.pointer.position,
                pointer_delta: state.pointer.delta,
                pointer_flags: state.pointer.flags.bits(),
                pointer_kind: state.pointer.kind as u8,
                accelerometer: state.sensor.accelerometer,
                gyroscope: state.sensor.gyroscope,
                orientation: state.sensor.orientation,
                has_orientation: state.sensor.has_orientation,
                sampled_at_ns: state.sensor.sampled_at_ns,
                battery_level: state.battery.level,
                charging: state.battery.charging,
            }));
    }

    pub fn on_host_input(&mut self, event: &HostInputEvent) {
        let message = match *event {
            HostInputEvent::Key {
                code,
                scan_code,
                virtual_key,
                pressed,
                repeat,
                extended,
                target_window,
                timestamp_ms,
                sequence,
            } => Some(InputMessage::HostKey(WireHostKey {
                code: code.wire(),
                target_window,
                virtual_key,
                scan_code,
                pressed,
                repeat,
                extended,
                timestamp_ms,
                sequence,
            })),
            HostInputEvent::Pointer(value) => Some(InputMessage::HostPointer(WireHostPointer {
                surface_id: value.surface_id,
                target_window: value.target_window,
                source: value.source,
                phase: value.phase,
                pointer_id: value.pointer_id,
                x: value.x,
                y: value.y,
                coordinate_width: value.coordinate_width,
                coordinate_height: value.coordinate_height,
                button: value.button,
                position: value.position.map(|axis| axis as f32),
                delta: value.delta.map(|axis| axis as f32),
                buttons: value.buttons,
                wheel_horizontal: value.wheel_horizontal,
                wheel_vertical: value.wheel_vertical,
                timestamp_ms: value.timestamp_ms,
            })),
            HostInputEvent::MouseMotion { dx, dy } => {
                Some(InputMessage::HostPointer(WireHostPointer {
                    surface_id: 0,
                    target_window: 0,
                    source: 1,
                    phase: 1,
                    pointer_id: 0,
                    x: 0,
                    y: 0,
                    coordinate_width: 0,
                    coordinate_height: 0,
                    button: 0,
                    position: [0.0; 2],
                    delta: [dx as f32, dy as f32],
                    buttons: 0,
                    wheel_horizontal: 0,
                    wheel_vertical: 0,
                    timestamp_ms: 0,
                }))
            }
            HostInputEvent::Wheel {
                horizontal,
                vertical,
            } => Some(InputMessage::HostWheel(WireHostWheel {
                horizontal,
                vertical,
            })),
            _ => None,
        };
        if let Some(message) = message {
            let _ = self.publisher.publish(message);
        }
    }
}
