use super::*;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::little_endian::{I32, I64, U16, U32, U64},
};

const MESSAGE_HELLO: u32 = 1;
const MESSAGE_STREAM_READY: u32 = 2;
const MESSAGE_SHUTDOWN: u32 = 4;
const MESSAGE_STREAM_ACCEPTED: u32 = 6;
const MESSAGE_BLOB: u32 = 8;
const MESSAGE_MAP_OFFER: u32 = 9;
const MESSAGE_MAP_ACCEPTED: u32 = 11;
const MESSAGE_GAMEPAD_FEEDBACK: u32 = 14;
const PAYLOAD_SIZE: usize = payload_size();

const fn payload_size() -> usize {
    let stream = mem::size_of::<WireStreamReady>();
    let map = mem::size_of::<WireMapOffer>();
    max(stream, map)
}

const fn max(left: usize, right: usize) -> usize {
    if left > right { left } else { right }
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(super) struct WireMessage {
    kind: U32,
    payload: [u8; PAYLOAD_SIZE],
}

// Keep allocations bounded before reading a variable-length control payload.
const MAX_CONTROL_DATA: usize = 8 * 1024 * 1024 - mem::size_of::<WireMessage>();
impl WireMessage {
    pub fn data_len(&self) -> io::Result<usize> {
        let len = if self.kind.get() == MESSAGE_BLOB {
            payload::<WireBlob>(self)?.len.get() as usize
        } else {
            0
        };
        if len > MAX_CONTROL_DATA {
            return Err(invalid_data("control payload is too large"));
        }
        Ok(len)
    }
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct WireHello {
    protocol_version: U32,
    process_id: U32,
    preferred_consumer: U32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct WireStreamReady {
    surface_id: U64,
    adapter_luid_low: U32,
    adapter_luid_high: I32,
    source_window: I64,
    hook: U32,
    width: U32,
    height: U32,
    format: U32,
    image_kind: U32,
    slot_count: U32,
    image_handles: [I64; MAX_IMAGE_SLOTS],
    timeline_semaphore_handle: I64,
    consumer_semaphore_handle: I64,
    bottom_up: U32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct WireGamepadFeedback {
    player: U32,
    low_frequency: U16,
    high_frequency: U16,
    duration_ms: U32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct WireBlob {
    kind: U32,
    len: U32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct WireMapOffer {
    channel: U32,
    handle: I64,
    mapping_len: U64,
    generation: U64,
}

pub(super) fn encode(message: &Message) -> io::Result<WireMessage> {
    let mut wire = WireMessage {
        kind: U32::ZERO,
        payload: [0; PAYLOAD_SIZE],
    };
    match message {
        Message::Hello(value) => {
            set_payload(
                &mut wire,
                MESSAGE_HELLO,
                &WireHello {
                    protocol_version: U32::new(value.protocol_version),
                    process_id: U32::new(value.process_id),
                    preferred_consumer: U32::new(value.preferred_consumer as u32),
                },
            );
        }
        Message::StreamReady(value) => {
            set_payload(
                &mut wire,
                MESSAGE_STREAM_READY,
                &WireStreamReady {
                    surface_id: U64::new(value.surface_id.0),
                    adapter_luid_low: U32::new(value.adapter_luid.low_part),
                    adapter_luid_high: I32::new(value.adapter_luid.high_part),
                    source_window: I64::new(value.source_window),
                    hook: U32::new(u32::from(value.hook.class())),
                    width: U32::new(value.width),
                    height: U32::new(value.height),
                    format: U32::new(value.format),
                    image_kind: U32::new(value.image_kind as u32),
                    slot_count: U32::new(value.slot_count),
                    image_handles: value.image_handles.map(I64::new),
                    timeline_semaphore_handle: I64::new(value.timeline_semaphore_handle),
                    consumer_semaphore_handle: I64::new(value.consumer_semaphore_handle),
                    bottom_up: U32::new(value.bottom_up as u32),
                },
            );
        }
        Message::Shutdown => {
            wire.kind = U32::new(MESSAGE_SHUTDOWN);
        }
        Message::GamepadFeedback(value) => {
            set_payload(
                &mut wire,
                MESSAGE_GAMEPAD_FEEDBACK,
                &WireGamepadFeedback {
                    player: U32::new(u32::from(value.player)),
                    low_frequency: U16::new(value.low_frequency),
                    high_frequency: U16::new(value.high_frequency),
                    duration_ms: U32::new(value.duration_ms),
                },
            );
        }
        Message::Blob { kind, data } => {
            if data.len() > MAX_CONTROL_DATA {
                return Err(invalid_data("control payload is too large"));
            }
            set_payload(
                &mut wire,
                MESSAGE_BLOB,
                &WireBlob {
                    kind: U32::new(*kind as u32),
                    len: U32::new(data.len() as u32),
                },
            );
        }
        Message::StreamAccepted => {
            wire.kind = U32::new(MESSAGE_STREAM_ACCEPTED);
        }
        Message::MapOffer(value) => {
            set_payload(
                &mut wire,
                MESSAGE_MAP_OFFER,
                &WireMapOffer {
                    channel: U32::new(value.channel as u32),
                    handle: I64::new(value.handle),
                    mapping_len: U64::new(value.mapping_len),
                    generation: U64::new(value.generation),
                },
            );
        }
        Message::MapAccepted => wire.kind = U32::new(MESSAGE_MAP_ACCEPTED),
    }
    Ok(wire)
}

fn set_payload<T: IntoBytes + Immutable>(wire: &mut WireMessage, kind: u32, payload: &T) {
    wire.kind = U32::new(kind);
    let bytes = payload.as_bytes();
    wire.payload[..bytes.len()].copy_from_slice(bytes);
}

pub(super) fn decode(wire: &WireMessage, data: Vec<u8>) -> io::Result<Message> {
    if data.len() != wire.data_len()? {
        return Err(invalid_data("control payload length mismatch"));
    }
    match wire.kind.get() {
        MESSAGE_HELLO => {
            let value = payload::<WireHello>(wire)?;
            let preferred_consumer =
                polymer_protocol::ConsumerApi::try_from(value.preferred_consumer.get())
                    .map_err(|()| invalid_data("invalid preferred consumer API"))?;
            Ok(Message::Hello(Handshake {
                protocol_version: value.protocol_version.get(),
                process_id: value.process_id.get(),
                preferred_consumer,
            }))
        }
        MESSAGE_STREAM_READY => {
            let value = payload::<WireStreamReady>(wire)?;
            let image_kind = ExternalImageKind::try_from(value.image_kind.get())
                .map_err(|()| io::Error::new(io::ErrorKind::InvalidData, "invalid image kind"))?;
            let hook = GraphicsHook::ALL
                .into_iter()
                .find(|hook| u32::from(hook.class()) == value.hook.get())
                .ok_or_else(|| invalid_data("invalid graphics hook"))?;
            Ok(Message::StreamReady(StreamReady {
                surface_id: SurfaceId(value.surface_id.get()),
                adapter_luid: AdapterLuid {
                    low_part: value.adapter_luid_low.get(),
                    high_part: value.adapter_luid_high.get(),
                },
                source_window: value.source_window.get(),
                hook,
                width: value.width.get(),
                height: value.height.get(),
                format: value.format.get(),
                image_kind,
                slot_count: value.slot_count.get(),
                image_handles: value.image_handles.map(I64::get),
                timeline_semaphore_handle: value.timeline_semaphore_handle.get(),
                consumer_semaphore_handle: value.consumer_semaphore_handle.get(),
                bottom_up: value.bottom_up.get() != 0,
            }))
        }
        MESSAGE_SHUTDOWN => Ok(Message::Shutdown),
        MESSAGE_STREAM_ACCEPTED => Ok(Message::StreamAccepted),
        MESSAGE_GAMEPAD_FEEDBACK => {
            let value = payload::<WireGamepadFeedback>(wire)?;
            let player = u8::try_from(value.player.get())
                .map_err(|_| invalid_data("invalid feedback player"))?;
            Ok(Message::GamepadFeedback(WireFeedback {
                player,
                low_frequency: value.low_frequency.get(),
                high_frequency: value.high_frequency.get(),
                duration_ms: value.duration_ms.get(),
            }))
        }
        MESSAGE_BLOB => {
            let value = payload::<WireBlob>(wire)?;
            let kind = match value.kind.get() {
                1 => BlobKind::Topology,
                2 => BlobKind::MacosAutomationEvent,
                _ => return Err(invalid_data("unknown blob kind")),
            };
            Ok(Message::Blob { kind, data })
        }
        MESSAGE_MAP_OFFER => {
            let value = payload::<WireMapOffer>(wire)?;
            let channel = match value.channel.get() {
                2 => ChannelKind::Input,
                _ => return Err(invalid_data("unknown shared channel kind")),
            };
            Ok(Message::MapOffer(MapOffer {
                channel,
                handle: value.handle.get(),
                mapping_len: value.mapping_len.get(),
                generation: value.generation.get(),
            }))
        }
        MESSAGE_MAP_ACCEPTED => Ok(Message::MapAccepted),
        _ => Err(invalid_data("unknown message type")),
    }
}

fn payload<T>(wire: &WireMessage) -> io::Result<&T>
where
    T: FromBytes + KnownLayout + Immutable + Unaligned,
{
    T::ref_from_prefix(&wire.payload)
        .map(|(value, _)| value)
        .map_err(|_| invalid_data("invalid message payload layout"))
}

pub(super) fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
