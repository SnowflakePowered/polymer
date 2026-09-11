use crate::{
    Message,
    control::{ChannelKind, LocalSocket, MapOffer},
    shared,
};
use polymer_protocol::{ConsumerApi, Handshake, PROTOCOL_VERSION};
use std::{io, mem};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::little_endian::{F32, I16, I32, I64, U16, U32, U64},
};

const DEVICE_CONNECTED: u8 = 1;
const DEVICE_DISCONNECTED: u8 = 2;
const DEVICE_STATE: u8 = 3;
const HOST_KEY: u8 = 4;
const HOST_POINTER: u8 = 5;
const HOST_WHEEL: u8 = 6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireDeviceConnected {
    pub index: u8,
    pub capabilities: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WireDeviceState {
    pub index: u8,
    pub sequence: u32,
    pub buttons: u32,
    pub pressure_mask: u32,
    pub pressure_values: [u8; 21],
    pub left_stick: [i16; 2],
    pub right_stick: [i16; 2],
    pub left_trigger: u16,
    pub right_trigger: u16,
    pub pointer_position: [i16; 2],
    pub pointer_delta: [i16; 2],
    pub pointer_flags: u8,
    pub pointer_kind: u8,
    pub accelerometer: [f32; 3],
    pub gyroscope: [f32; 3],
    pub orientation: [f32; 4],
    pub has_orientation: bool,
    pub sampled_at_ns: u64,
    pub battery_level: Option<u8>,
    pub charging: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireHostKey {
    pub code: u16,
    pub target_window: i64,
    pub virtual_key: u16,
    pub scan_code: u16,
    pub pressed: bool,
    pub repeat: bool,
    pub extended: bool,
    pub timestamp_ms: u32,
    pub sequence: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WireHostPointer {
    pub surface_id: u64,
    pub target_window: i64,
    pub source: u8,
    pub phase: u8,
    pub pointer_id: u32,
    pub x: i32,
    pub y: i32,
    pub coordinate_width: u32,
    pub coordinate_height: u32,
    pub button: u32,
    pub position: [f32; 2],
    pub delta: [f32; 2],
    pub buttons: u32,
    pub wheel_horizontal: i32,
    pub wheel_vertical: i32,
    pub timestamp_ms: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireHostWheel {
    pub horizontal: i32,
    pub vertical: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InputMessage {
    DeviceConnected(WireDeviceConnected),
    DeviceDisconnected { index: u8 },
    DeviceState(WireDeviceState),
    HostKey(WireHostKey),
    HostPointer(WireHostPointer),
    HostWheel(WireHostWheel),
}

#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
struct WireInputMessage {
    kind: u8,
    index: u8,
    flags: U16,
    sequence: U32,
    buttons: U32,
    values_i16: [I16; 12],
    values_u16: [U16; 2],
    values_f32: [F32; 10],
    sampled_at_ns: U64,
    target_window: I64,
    timestamp_ms: U32,
    horizontal: I32,
    vertical: I32,
    capabilities: U32,
    pressure_mask: U32,
    pressure_values: [u8; 21],
}

impl WireInputMessage {
    fn empty(kind: u8, index: u8) -> Self {
        Self {
            kind,
            index,
            flags: U16::ZERO,
            sequence: U32::ZERO,
            buttons: U32::ZERO,
            values_i16: [I16::ZERO; 12],
            values_u16: [U16::ZERO; 2],
            values_f32: [F32::ZERO; 10],
            sampled_at_ns: U64::ZERO,
            target_window: I64::ZERO,
            timestamp_ms: U32::ZERO,
            horizontal: I32::ZERO,
            vertical: I32::ZERO,
            capabilities: U32::ZERO,
            pressure_mask: U32::ZERO,
            pressure_values: [0; 21],
        }
    }

    fn encode(value: InputMessage) -> Self {
        match value {
            InputMessage::DeviceConnected(value) => {
                let mut wire = Self::empty(DEVICE_CONNECTED, value.index);
                wire.capabilities = U32::new(value.capabilities);
                wire
            }
            InputMessage::DeviceDisconnected { index } => Self::empty(DEVICE_DISCONNECTED, index),
            InputMessage::DeviceState(value) => Self::encode_state(value),
            InputMessage::HostKey(value) => {
                let mut wire = Self::empty(HOST_KEY, 0);
                wire.target_window = I64::new(value.target_window);
                wire.values_u16 = [U16::new(value.virtual_key), U16::new(value.scan_code)];
                wire.values_i16[0] = I16::new(value.code as i16);
                wire.flags = U16::new(
                    u16::from(value.pressed)
                        | u16::from(value.repeat) << 1
                        | u16::from(value.extended) << 2,
                );
                wire.timestamp_ms = U32::new(value.timestamp_ms);
                wire.sequence = U32::new(value.sequence);
                wire
            }
            InputMessage::HostPointer(value) => {
                let mut wire = Self::empty(HOST_POINTER, 0);
                wire.index = value.source;
                wire.flags = U16::new(u16::from(value.phase));
                wire.sampled_at_ns = U64::new(value.surface_id);
                wire.target_window = I64::new(value.target_window);
                wire.capabilities = U32::new(value.pointer_id);
                wire.values_i16[..8].copy_from_slice(&[
                    Self::low(value.x as u32),
                    Self::high(value.x as u32),
                    Self::low(value.y as u32),
                    Self::high(value.y as u32),
                    Self::low(value.coordinate_width),
                    Self::high(value.coordinate_width),
                    Self::low(value.coordinate_height),
                    Self::high(value.coordinate_height),
                ]);
                wire.values_u16[0] = U16::new(value.button as u16);
                wire.values_f32[..4].copy_from_slice(
                    &[
                        value.position[0],
                        value.position[1],
                        value.delta[0],
                        value.delta[1],
                    ]
                    .map(F32::new),
                );
                wire.buttons = U32::new(value.buttons);
                wire.horizontal = I32::new(value.wheel_horizontal);
                wire.vertical = I32::new(value.wheel_vertical);
                wire.timestamp_ms = U32::new(value.timestamp_ms);
                wire
            }
            InputMessage::HostWheel(value) => {
                let mut wire = Self::empty(HOST_WHEEL, 0);
                wire.horizontal = I32::new(value.horizontal);
                wire.vertical = I32::new(value.vertical);
                wire
            }
        }
    }

    fn encode_state(value: WireDeviceState) -> Self {
        let mut wire = Self::empty(DEVICE_STATE, value.index);
        wire.sequence = U32::new(value.sequence);
        wire.buttons = U32::new(value.buttons);
        wire.pressure_mask = U32::new(value.pressure_mask);
        wire.pressure_values = value.pressure_values;
        wire.values_i16 = [
            value.left_stick[0],
            value.left_stick[1],
            value.right_stick[0],
            value.right_stick[1],
            value.pointer_position[0],
            value.pointer_position[1],
            value.pointer_delta[0],
            value.pointer_delta[1],
            i16::from(value.battery_level.unwrap_or(0)),
            0,
            0,
            0,
        ]
        .map(I16::new);
        wire.values_u16 = [U16::new(value.left_trigger), U16::new(value.right_trigger)];
        wire.values_f32 = [
            value.accelerometer[0],
            value.accelerometer[1],
            value.accelerometer[2],
            value.gyroscope[0],
            value.gyroscope[1],
            value.gyroscope[2],
            value.orientation[0],
            value.orientation[1],
            value.orientation[2],
            value.orientation[3],
        ]
        .map(F32::new);
        wire.flags = U16::new(
            u16::from(value.pointer_flags)
                | u16::from(value.pointer_kind) << 8
                | u16::from(value.has_orientation) << 12
                | u16::from(value.charging) << 13
                | u16::from(value.battery_level.is_some()) << 14,
        );
        wire.sampled_at_ns = U64::new(value.sampled_at_ns);
        wire
    }

    fn decode(&self) -> io::Result<InputMessage> {
        match self.kind {
            DEVICE_CONNECTED => Ok(InputMessage::DeviceConnected(WireDeviceConnected {
                index: self.index,
                capabilities: self.capabilities.get(),
            })),
            DEVICE_DISCONNECTED => Ok(InputMessage::DeviceDisconnected { index: self.index }),
            DEVICE_STATE => Ok(InputMessage::DeviceState(self.device_state())),
            HOST_KEY => Ok(InputMessage::HostKey(WireHostKey {
                code: self.values_i16[0].get() as u16,
                target_window: self.target_window.get(),
                virtual_key: self.values_u16[0].get(),
                scan_code: self.values_u16[1].get(),
                pressed: self.flags.get() & 1 != 0,
                repeat: self.flags.get() & 2 != 0,
                extended: self.flags.get() & 4 != 0,
                timestamp_ms: self.timestamp_ms.get(),
                sequence: self.sequence.get(),
            })),
            HOST_POINTER => Ok(InputMessage::HostPointer(WireHostPointer {
                surface_id: self.sampled_at_ns.get(),
                target_window: self.target_window.get(),
                source: self.index,
                phase: self.flags.get() as u8,
                pointer_id: self.capabilities.get(),
                x: Self::join(self.values_i16[0], self.values_i16[1]) as i32,
                y: Self::join(self.values_i16[2], self.values_i16[3]) as i32,
                coordinate_width: Self::join(self.values_i16[4], self.values_i16[5]),
                coordinate_height: Self::join(self.values_i16[6], self.values_i16[7]),
                button: u32::from(self.values_u16[0].get()),
                position: [self.values_f32[0].get(), self.values_f32[1].get()],
                delta: [self.values_f32[2].get(), self.values_f32[3].get()],
                buttons: self.buttons.get(),
                wheel_horizontal: self.horizontal.get(),
                wheel_vertical: self.vertical.get(),
                timestamp_ms: self.timestamp_ms.get(),
            })),
            HOST_WHEEL => Ok(InputMessage::HostWheel(WireHostWheel {
                horizontal: self.horizontal.get(),
                vertical: self.vertical.get(),
            })),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unknown input event",
            )),
        }
    }

    fn device_state(&self) -> WireDeviceState {
        let flags = self.flags.get();
        let accelerometer = [
            self.values_f32[0].get(),
            self.values_f32[1].get(),
            self.values_f32[2].get(),
        ];
        let gyroscope = [
            self.values_f32[3].get(),
            self.values_f32[4].get(),
            self.values_f32[5].get(),
        ];
        let orientation = [
            self.values_f32[6].get(),
            self.values_f32[7].get(),
            self.values_f32[8].get(),
            self.values_f32[9].get(),
        ];
        WireDeviceState {
            index: self.index,
            sequence: self.sequence.get(),
            buttons: self.buttons.get(),
            pressure_mask: self.pressure_mask.get(),
            pressure_values: self.pressure_values,
            left_stick: [self.values_i16[0].get(), self.values_i16[1].get()],
            right_stick: [self.values_i16[2].get(), self.values_i16[3].get()],
            left_trigger: self.values_u16[0].get(),
            right_trigger: self.values_u16[1].get(),
            pointer_position: [self.values_i16[4].get(), self.values_i16[5].get()],
            pointer_delta: [self.values_i16[6].get(), self.values_i16[7].get()],
            pointer_flags: flags as u8,
            pointer_kind: (flags >> 8) as u8 & 0x0f,
            accelerometer,
            gyroscope,
            orientation,
            has_orientation: flags & (1 << 12) != 0,
            sampled_at_ns: self.sampled_at_ns.get(),
            battery_level: (flags & (1 << 14) != 0).then_some(self.values_i16[8].get() as u8),
            charging: flags & (1 << 13) != 0,
        }
    }

    fn low(value: u32) -> I16 {
        I16::new(value as u16 as i16)
    }

    fn high(value: u32) -> I16 {
        I16::new((value >> 16) as u16 as i16)
    }

    fn join(low: I16, high: I16) -> u32 {
        u32::from(low.get() as u16) | u32::from(high.get() as u16) << 16
    }
}

pub struct InputPublisher {
    shared: shared::Publisher,
    generation: u64,
    latest: [Option<WireInputMessage>; 8],
    connected: [Option<WireInputMessage>; 8],
    scratch: WireInputMessage,
}

impl InputPublisher {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            shared: shared::Publisher::create("input")?,
            generation: 1,
            latest: [None; 8],
            connected: [None; 8],
            scratch: WireInputMessage::empty(0, 0),
        })
    }

    pub fn accept(&mut self, control: &mut LocalSocket) -> io::Result<u32> {
        let Message::Hello(hello) = control.receive()? else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected input hello",
            ));
        };
        if hello.protocol_version != PROTOCOL_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "input protocol mismatch",
            ));
        }
        let handle = self.shared.duplicate_for(hello.process_id)?;
        control.send(Message::MapOffer(MapOffer {
            channel: ChannelKind::Input,
            handle,
            mapping_len: self.shared.mapping_len(),
            generation: self.generation,
        }))?;
        if control.receive()? != Message::MapAccepted {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "input map was not accepted",
            ));
        }
        for wire in self
            .connected
            .iter()
            .flatten()
            .chain(self.latest.iter().flatten())
        {
            self.shared.publish(wire.as_bytes())?;
        }
        Ok(hello.process_id)
    }

    pub fn publish(&mut self, message: InputMessage) -> io::Result<()> {
        self.scratch = WireInputMessage::encode(message);
        let index = usize::from(self.scratch.index);
        match self.scratch.kind {
            DEVICE_CONNECTED if index < 8 => self.connected[index] = Some(self.scratch),
            DEVICE_DISCONNECTED if index < 8 => {
                self.connected[index] = Some(self.scratch);
                self.latest[index] = None;
            }
            DEVICE_STATE if index < 8 => self.latest[index] = Some(self.scratch),
            _ => {}
        }
        self.shared.publish(self.scratch.as_bytes())
    }
}

pub struct InputSubscriber {
    pub control: LocalSocket,
    shared: shared::Subscriber,
    scratch: Vec<u8>,
}

impl InputSubscriber {
    pub fn connect(name: &str) -> io::Result<Self> {
        let mut control = LocalSocket::connect(name)?;
        control.send(Message::Hello(Handshake {
            protocol_version: PROTOCOL_VERSION,
            process_id: std::process::id(),
            preferred_consumer: ConsumerApi::Vulkan,
        }))?;
        let Message::MapOffer(offer) = control.receive()? else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected input map offer",
            ));
        };
        if offer.channel != ChannelKind::Input {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "wrong shared channel kind",
            ));
        }
        let shared = unsafe { shared::Subscriber::from_handle(offer.handle, offer.mapping_len)? };
        control.send(Message::MapAccepted)?;
        Ok(Self {
            control,
            shared,
            scratch: Vec::with_capacity(mem::size_of::<WireInputMessage>()),
        })
    }

    pub fn drain(&mut self, mut receive: impl FnMut(InputMessage)) -> io::Result<usize> {
        self.shared.drain(&mut self.scratch, |bytes| {
            let wire = WireInputMessage::read_from_bytes(bytes)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid input frame"))?;
            receive(wire.decode()?);
            Ok(())
        })
    }
}

const _: () = assert!(mem::size_of::<WireInputMessage>() <= 160);

#[cfg(feature = "input-host")]
pub mod host;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_and_motion_survive_wire_serialization() {
        let mut state = WireInputMessage::empty(DEVICE_STATE, 3).device_state();
        state.sequence = 42;
        state.buttons = 0x7fff;
        state.pressure_mask = 0x7e0f;
        state.pressure_values = std::array::from_fn(|index| (index * 11) as u8);
        state.left_trigger = 32896;
        state.right_trigger = 65535;
        state.accelerometer = [1.0, -2.0, 9.80665];
        state.gyroscope = [0.0, -1.5, 0.0];
        state.sampled_at_ns = 123456;
        state.battery_level = Some(75);
        let message = InputMessage::DeviceState(state);
        let encoded = WireInputMessage::encode(message);
        let read = WireInputMessage::read_from_bytes(encoded.as_bytes()).unwrap();
        assert_eq!(read.decode().unwrap(), message);
        assert!(WireInputMessage::read_from_bytes(&encoded.as_bytes()[..112]).is_err());
    }
}
