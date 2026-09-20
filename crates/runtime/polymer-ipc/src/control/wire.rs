use super::*;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::little_endian::{I32, I64, U16, U32, U64},
};

const MESSAGE_HELLO: u32 = 1;
const MESSAGE_STREAM_READY: u32 = 2;
const MESSAGE_FRAME_READY: u32 = 3;
const MESSAGE_SHUTDOWN: u32 = 4;
const MESSAGE_STREAM_ACCEPTED: u32 = 6;
const MESSAGE_BLOB: u32 = 8;
const MESSAGE_MAP_OFFER: u32 = 9;
const MESSAGE_FRAME_AVAILABLE: u32 = 10;
const MESSAGE_MAP_ACCEPTED: u32 = 11;
const MESSAGE_RESIZE_WINDOW: u32 = 13;
const MESSAGE_GAMEPAD_FEEDBACK: u32 = 14;
const MESSAGE_BUFFER_AVAILABLE: u32 = 15;
const MESSAGE_REQUEST_BUFFER_AVAILABILITY: u32 = 16;
const PAYLOAD_SIZE: usize = payload_size();

const fn payload_size() -> usize {
    let stream = mem::size_of::<WireStreamReady>();
    let frame = mem::size_of::<WireFrameReady>();
    let map = mem::size_of::<WireMapOffer>();
    max(max(stream, frame), map)
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
struct WireBufferAvailable {
    stream_id: U64,
    through_sync_value: U64,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct WireStreamReady {
    producer_process_id: U32,
    surface_id: U64,
    adapter_luid_low: U32,
    adapter_luid_high: I32,
    source_window: I64,
    width: U32,
    height: U32,
    format: U32,
    image_kind: U32,
    sync_kind: U32,
    slot_count: U32,
    image_handles: [I64; MAX_IMAGE_SLOTS],
    timeline_semaphore_handle: I64,
    consumer_semaphore_handle: I64,
    bottom_up: U32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct WireFrameReady {
    stream_id: U64,
    slot: U32,
    render_width: U32,
    render_height: U32,
    capture_x: U32,
    capture_y: U32,
    capture_width: U32,
    capture_height: U32,
    presentation_width: U32,
    presentation_height: U32,
    mapping_kind: U32,
    upper_left_x: I64,
    upper_left_y: I64,
    upper_right_x: I64,
    upper_right_y: I64,
    lower_left_x: I64,
    lower_left_y: I64,
    frame_number: U64,
    sync_value: U64,
    timestamp_qpc: U64,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct WireResizeWindow {
    target_window: I64,
    width: U32,
    height: U32,
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
                    producer_process_id: U32::new(value.producer_process_id),
                    surface_id: U64::new(value.surface_id.0),
                    adapter_luid_low: U32::new(value.adapter_luid.low_part),
                    adapter_luid_high: I32::new(value.adapter_luid.high_part),
                    source_window: I64::new(value.source_window),
                    width: U32::new(value.width),
                    height: U32::new(value.height),
                    format: U32::new(value.format),
                    image_kind: U32::new(value.image_kind as u32),
                    sync_kind: U32::new(value.sync_kind as u32),
                    slot_count: U32::new(value.slot_count),
                    image_handles: value.image_handles.map(I64::new),
                    timeline_semaphore_handle: I64::new(value.timeline_semaphore_handle),
                    consumer_semaphore_handle: I64::new(value.consumer_semaphore_handle),
                    bottom_up: U32::new(value.bottom_up as u32),
                },
            );
        }
        Message::FrameReady(value) => {
            set_payload(
                &mut wire,
                MESSAGE_FRAME_READY,
                &WireFrameReady {
                    stream_id: U64::new(value.stream_id.0),
                    slot: U32::new(value.slot),
                    render_width: U32::new(value.render_width),
                    render_height: U32::new(value.render_height),
                    capture_x: U32::new(value.capture_x),
                    capture_y: U32::new(value.capture_y),
                    capture_width: U32::new(value.capture_width),
                    capture_height: U32::new(value.capture_height),
                    presentation_width: U32::new(value.presentation_width),
                    presentation_height: U32::new(value.presentation_height),
                    mapping_kind: U32::new(value.presentation_transform.kind as u32),
                    upper_left_x: I64::new(value.presentation_transform.upper_left.x),
                    upper_left_y: I64::new(value.presentation_transform.upper_left.y),
                    upper_right_x: I64::new(value.presentation_transform.upper_right.x),
                    upper_right_y: I64::new(value.presentation_transform.upper_right.y),
                    lower_left_x: I64::new(value.presentation_transform.lower_left.x),
                    lower_left_y: I64::new(value.presentation_transform.lower_left.y),
                    frame_number: U64::new(value.frame_number),
                    sync_value: U64::new(value.sync_value),
                    timestamp_qpc: U64::new(value.timestamp_qpc),
                },
            );
        }
        Message::Shutdown => {
            wire.kind = U32::new(MESSAGE_SHUTDOWN);
        }
        Message::ResizeWindow(value) => {
            set_payload(
                &mut wire,
                MESSAGE_RESIZE_WINDOW,
                &WireResizeWindow {
                    target_window: I64::new(value.target_window),
                    width: U32::new(value.width),
                    height: U32::new(value.height),
                },
            );
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
        Message::RequestBufferAvailability => {
            wire.kind = U32::new(MESSAGE_REQUEST_BUFFER_AVAILABILITY);
        }
        Message::BufferAvailable {
            stream_id,
            through_sync_value,
        } => {
            set_payload(
                &mut wire,
                MESSAGE_BUFFER_AVAILABLE,
                &WireBufferAvailable {
                    stream_id: U64::new(stream_id.0),
                    through_sync_value: U64::new(*through_sync_value),
                },
            );
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
        Message::FrameAvailable => wire.kind = U32::new(MESSAGE_FRAME_AVAILABLE),
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
            let sync_kind = ExternalSyncKind::try_from(value.sync_kind.get())
                .map_err(|()| io::Error::new(io::ErrorKind::InvalidData, "invalid sync kind"))?;
            Ok(Message::StreamReady(StreamReady {
                producer_process_id: value.producer_process_id.get(),
                surface_id: SurfaceId(value.surface_id.get()),
                adapter_luid: AdapterLuid {
                    low_part: value.adapter_luid_low.get(),
                    high_part: value.adapter_luid_high.get(),
                },
                source_window: value.source_window.get(),
                width: value.width.get(),
                height: value.height.get(),
                format: value.format.get(),
                image_kind,
                sync_kind,
                slot_count: value.slot_count.get(),
                image_handles: value.image_handles.map(I64::get),
                timeline_semaphore_handle: value.timeline_semaphore_handle.get(),
                consumer_semaphore_handle: value.consumer_semaphore_handle.get(),
                bottom_up: value.bottom_up.get() != 0,
            }))
        }
        MESSAGE_FRAME_READY => {
            let value = payload::<WireFrameReady>(wire)?;
            let mapping_kind = PresentationMappingKind::try_from(value.mapping_kind.get())
                .map_err(|()| invalid_data("invalid presentation mapping kind"))?;
            Ok(Message::FrameReady(FrameReady {
                stream_id: StreamId(value.stream_id.get()),
                slot: value.slot.get(),
                render_width: value.render_width.get(),
                render_height: value.render_height.get(),
                capture_x: value.capture_x.get(),
                capture_y: value.capture_y.get(),
                capture_width: value.capture_width.get(),
                capture_height: value.capture_height.get(),
                presentation_width: value.presentation_width.get(),
                presentation_height: value.presentation_height.get(),
                presentation_transform: PresentationTransform {
                    kind: mapping_kind,
                    upper_left: PresentationPoint {
                        x: value.upper_left_x.get(),
                        y: value.upper_left_y.get(),
                    },
                    upper_right: PresentationPoint {
                        x: value.upper_right_x.get(),
                        y: value.upper_right_y.get(),
                    },
                    lower_left: PresentationPoint {
                        x: value.lower_left_x.get(),
                        y: value.lower_left_y.get(),
                    },
                },
                frame_number: value.frame_number.get(),
                sync_value: value.sync_value.get(),
                timestamp_qpc: value.timestamp_qpc.get(),
            }))
        }
        MESSAGE_SHUTDOWN => Ok(Message::Shutdown),
        MESSAGE_RESIZE_WINDOW => {
            let value = payload::<WireResizeWindow>(wire)?;
            Ok(Message::ResizeWindow(ResizeWindow {
                target_window: value.target_window.get(),
                width: value.width.get(),
                height: value.height.get(),
            }))
        }
        MESSAGE_STREAM_ACCEPTED => Ok(Message::StreamAccepted),
        MESSAGE_REQUEST_BUFFER_AVAILABILITY => Ok(Message::RequestBufferAvailability),
        MESSAGE_BUFFER_AVAILABLE => {
            let value = payload::<WireBufferAvailable>(wire)?;
            Ok(Message::BufferAvailable {
                stream_id: StreamId(value.stream_id.get()),
                through_sync_value: value.through_sync_value.get(),
            })
        }
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
                1 => ChannelKind::Frame,
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
        MESSAGE_FRAME_AVAILABLE => Ok(Message::FrameAvailable),
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

pub(crate) fn encode_frame(value: FrameReady) -> Vec<u8> {
    let wire = encode(&Message::FrameReady(value)).expect("fixed-size frame encoding");
    wire.payload[..mem::size_of::<WireFrameReady>()].to_vec()
}

pub(crate) fn decode_frame(bytes: &[u8]) -> io::Result<FrameReady> {
    let mut wire = WireMessage {
        kind: U32::new(MESSAGE_FRAME_READY),
        payload: [0; PAYLOAD_SIZE],
    };
    if bytes.len() != mem::size_of::<WireFrameReady>() {
        return Err(invalid_data("invalid shared frame size"));
    }
    wire.payload[..bytes.len()].copy_from_slice(bytes);
    match decode(&wire, Vec::new())? {
        Message::FrameReady(value) => Ok(value),
        _ => unreachable!(),
    }
}
