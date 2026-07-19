//! Framed Snowglass IPC over a Windows AF_UNIX stream socket.

use snowglass_input::{
    DirectInputData, DirectInputDevice,
    mouse::{PointerInput, PointerPhase, PointerSource},
};
use snowglass_protocol::{
    AdapterLuid, ExternalImageKind, ExternalSyncKind, FrameReady, Handshake, MAX_IMAGE_SLOTS,
    PresentationMappingKind, PresentationPoint, PresentationTransform, StreamId, SurfaceId,
};
use std::{
    fs,
    io::{self, Read, Write},
    mem,
    net::TcpStream,
    os::windows::io::FromRawSocket,
    path::PathBuf,
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::Networking::WinSock::{
    AF_UNIX, INVALID_SOCKET, SOCK_STREAM, SOCKADDR, SOCKADDR_UN, SOCKET, SOMAXCONN, WSADATA,
    WSAGetLastError, WSAStartup, accept, bind, closesocket, connect, listen, socket,
};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::little_endian::{I32, I64, U32, U64},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(1);
const WINSOCK_VERSION_2_2: u16 = 0x0202;
const MESSAGE_HELLO: u32 = 1;
const MESSAGE_STREAM_READY: u32 = 2;
const MESSAGE_FRAME_READY: u32 = 3;
const MESSAGE_SHUTDOWN: u32 = 4;
const MESSAGE_DIRECT_INPUT_DATA: u32 = 5;
const MESSAGE_STREAM_ACCEPTED: u32 = 6;
const MESSAGE_POINTER_INPUT: u32 = 7;
const PAYLOAD_SIZE: usize = payload_size();

const fn payload_size() -> usize {
    let stream = mem::size_of::<WireStreamReady>();
    let frame = mem::size_of::<WireFrameReady>();
    let pointer = mem::size_of::<WirePointerInput>();
    if stream > frame {
        if stream > pointer { stream } else { pointer }
    } else if frame > pointer {
        frame
    } else {
        pointer
    }
}

static WINSOCK: OnceLock<i32> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamReady {
    pub producer_process_id: u32,
    /// Opaque logical surface identity. Unlike `source_window`, this may be
    /// synthesized to distinguish multiple frame-graph surfaces from one HWND.
    pub surface_id: SurfaceId,
    pub adapter_luid: AdapterLuid,
    /// Real Win32 HWND used as the target for pointer and keyboard messages.
    pub source_window: i64,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub image_kind: ExternalImageKind,
    pub sync_kind: ExternalSyncKind,
    pub slot_count: u32,
    /// Handles already duplicated into the viewer process.
    pub image_handles: [i64; MAX_IMAGE_SLOTS],
    pub timeline_semaphore_handle: i64,
    /// Optional timeline handle signaled by the viewer after consuming a slot.
    pub consumer_semaphore_handle: i64,
    /// Pixel rows are stored bottom-up (OpenGL default-framebuffer origin);
    /// the consumer flips vertically while presenting. Producers pass
    /// captures through verbatim and describe the orientation instead.
    pub bottom_up: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Message {
    Hello(Handshake),
    StreamReady(StreamReady),
    FrameReady(FrameReady),
    Shutdown,
    DirectInputData(DirectInputData),
    PointerInput(PointerInput),
    /// The viewer has imported the stream resources and released its previous stream.
    StreamAccepted,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct WireMessage {
    kind: U32,
    payload: [u8; PAYLOAD_SIZE],
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
struct WireDirectInputData {
    device: U32,
    offset: U32,
    data: U32,
    timestamp: U32,
    sequence: U32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct WirePointerInput {
    surface_id: U64,
    target_window: I64,
    frame_number: U64,
    source: U32,
    phase: U32,
    pointer_id: U32,
    x: I32,
    y: I32,
    coordinate_width: U32,
    coordinate_height: U32,
    button: U32,
    buttons: U32,
    wheel_horizontal: I32,
    wheel_vertical: I32,
    timestamp: U32,
    sequence: U32,
}

pub struct LocalSocketListener {
    socket: SOCKET,
    path: PathBuf,
}

// SAFETY: the listener exclusively owns its socket. Moving the socket between
// threads does not duplicate access or invalidate it.
unsafe impl Send for LocalSocketListener {}

impl LocalSocketListener {
    pub fn bind(name: &str) -> io::Result<Self> {
        initialize_winsock()?;
        let (address, path) = socket_address(name)?;
        let socket = create_socket()?;
        // SAFETY: address points to an initialized SOCKADDR_UN for the duration
        // of the call, and socket is a live AF_UNIX stream socket.
        if unsafe {
            bind(
                socket,
                (&raw const address).cast::<SOCKADDR>(),
                mem::size_of::<SOCKADDR_UN>() as i32,
            )
        } != 0
        {
            let error = winsock_error();
            unsafe { closesocket(socket) };
            return Err(error);
        }
        // SAFETY: socket was successfully bound above.
        if unsafe { listen(socket, SOMAXCONN as i32) } != 0 {
            let error = winsock_error();
            unsafe { closesocket(socket) };
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        Ok(Self { socket, path })
    }

    pub fn accept(&self) -> io::Result<LocalSocket> {
        // SAFETY: self.socket is a listening socket. Peer address information is
        // not needed for this local, single-client transport.
        let connected = unsafe { accept(self.socket, std::ptr::null_mut(), std::ptr::null_mut()) };
        if connected == INVALID_SOCKET {
            return Err(winsock_error());
        }
        // SAFETY: ownership of the accepted Winsock socket moves into TcpStream.
        let stream = unsafe { TcpStream::from_raw_socket(connected as _) };
        Ok(LocalSocket { stream })
    }
}

impl Drop for LocalSocketListener {
    fn drop(&mut self) {
        if self.socket != INVALID_SOCKET {
            // SAFETY: this object exclusively owns the live listening socket.
            unsafe { closesocket(self.socket) };
            self.socket = INVALID_SOCKET;
        }
        let _ = fs::remove_file(&self.path);
    }
}

pub struct LocalSocket {
    stream: TcpStream,
}

impl LocalSocket {
    pub fn connect(name: &str) -> io::Result<Self> {
        initialize_winsock()?;
        let (address, _) = socket_address(name)?;
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        loop {
            let socket = create_socket()?;
            // SAFETY: address points to an initialized SOCKADDR_UN and socket is
            // a live AF_UNIX stream socket.
            if unsafe {
                connect(
                    socket,
                    (&raw const address).cast::<SOCKADDR>(),
                    mem::size_of::<SOCKADDR_UN>() as i32,
                )
            } == 0
            {
                // SAFETY: ownership of the connected socket moves into TcpStream.
                let stream = unsafe { TcpStream::from_raw_socket(socket as _) };
                return Ok(Self { stream });
            }
            let error = winsock_error();
            unsafe { closesocket(socket) };
            if Instant::now() >= deadline {
                return Err(error);
            }
            thread::sleep(CONNECT_RETRY_DELAY);
        }
    }

    pub fn send(&mut self, message: Message) -> io::Result<()> {
        self.stream.write_all(encode(message).as_bytes())
    }

    pub fn receive(&mut self) -> io::Result<Message> {
        let mut bytes = [0; mem::size_of::<WireMessage>()];
        self.stream.read_exact(&mut bytes)?;
        let wire = WireMessage::ref_from_bytes(&bytes)
            .map_err(|_| invalid_data("invalid wire message layout"))?;
        decode(wire)
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }
}

fn initialize_winsock() -> io::Result<()> {
    let result = *WINSOCK.get_or_init(|| {
        let mut data = WSADATA::default();
        // SAFETY: data is valid writable storage for WSADATA.
        unsafe { WSAStartup(WINSOCK_VERSION_2_2, &raw mut data) }
    });
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result))
    }
}

fn create_socket() -> io::Result<SOCKET> {
    // SAFETY: the address family, socket type, and default protocol are valid.
    let socket = unsafe { socket(AF_UNIX as i32, SOCK_STREAM, 0) };
    if socket == INVALID_SOCKET {
        Err(winsock_error())
    } else {
        Ok(socket)
    }
}

fn socket_address(name: &str) -> io::Result<(SOCKADDR_UN, PathBuf)> {
    if name.is_empty() || name.contains(['/', '\\']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "local socket name must be a non-empty file name",
        ));
    }
    let path = std::env::temp_dir().join(format!("{name}.sock"));
    let path_bytes = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non-Unicode socket path"))?
        .as_bytes();
    let mut address = SOCKADDR_UN {
        sun_family: AF_UNIX,
        sun_path: [0; 108],
    };
    if path_bytes.len() >= address.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "local socket path is too long",
        ));
    }
    for (destination, source) in address.sun_path.iter_mut().zip(path_bytes) {
        *destination = *source as i8;
    }
    Ok((address, path))
}

fn winsock_error() -> io::Error {
    // SAFETY: WSAGetLastError has no preconditions.
    io::Error::from_raw_os_error(unsafe { WSAGetLastError() })
}

fn encode(message: Message) -> WireMessage {
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
        Message::DirectInputData(value) => {
            set_payload(
                &mut wire,
                MESSAGE_DIRECT_INPUT_DATA,
                &WireDirectInputData {
                    device: U32::new(value.device as u32),
                    offset: U32::new(value.offset),
                    data: U32::new(value.data),
                    timestamp: U32::new(value.timestamp),
                    sequence: U32::new(value.sequence),
                },
            );
        }
        Message::PointerInput(value) => {
            set_payload(
                &mut wire,
                MESSAGE_POINTER_INPUT,
                &WirePointerInput {
                    surface_id: U64::new(value.surface_id),
                    target_window: I64::new(value.target_window),
                    frame_number: U64::new(value.frame_number),
                    source: U32::new(value.source as u32),
                    phase: U32::new(value.phase as u32),
                    pointer_id: U32::new(value.pointer_id),
                    x: I32::new(value.x),
                    y: I32::new(value.y),
                    coordinate_width: U32::new(value.coordinate_width),
                    coordinate_height: U32::new(value.coordinate_height),
                    button: U32::new(value.button),
                    buttons: U32::new(value.buttons),
                    wheel_horizontal: I32::new(value.wheel_horizontal),
                    wheel_vertical: I32::new(value.wheel_vertical),
                    timestamp: U32::new(value.timestamp),
                    sequence: U32::new(value.sequence),
                },
            );
        }
        Message::StreamAccepted => {
            wire.kind = U32::new(MESSAGE_STREAM_ACCEPTED);
        }
    }
    wire
}

fn set_payload<T: IntoBytes + Immutable>(wire: &mut WireMessage, kind: u32, payload: &T) {
    wire.kind = U32::new(kind);
    let bytes = payload.as_bytes();
    wire.payload[..bytes.len()].copy_from_slice(bytes);
}

fn decode(wire: &WireMessage) -> io::Result<Message> {
    match wire.kind.get() {
        MESSAGE_HELLO => {
            let value = payload::<WireHello>(wire)?;
            let preferred_consumer =
                snowglass_protocol::ConsumerApi::try_from(value.preferred_consumer.get())
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
        MESSAGE_DIRECT_INPUT_DATA => {
            let value = payload::<WireDirectInputData>(wire)?;
            let device = match value.device.get() {
                1 => DirectInputDevice::Keyboard,
                2 => DirectInputDevice::Mouse,
                _ => return Err(invalid_data("unknown DirectInput device class")),
            };
            Ok(Message::DirectInputData(DirectInputData {
                device,
                offset: value.offset.get(),
                data: value.data.get(),
                timestamp: value.timestamp.get(),
                sequence: value.sequence.get(),
            }))
        }
        MESSAGE_STREAM_ACCEPTED => Ok(Message::StreamAccepted),
        MESSAGE_POINTER_INPUT => {
            let value = payload::<WirePointerInput>(wire)?;
            let source = PointerSource::try_from(value.source.get())
                .map_err(|()| invalid_data("invalid pointer source"))?;
            let phase = PointerPhase::try_from(value.phase.get())
                .map_err(|()| invalid_data("invalid pointer phase"))?;
            Ok(Message::PointerInput(PointerInput {
                surface_id: value.surface_id.get(),
                target_window: value.target_window.get(),
                frame_number: value.frame_number.get(),
                source,
                phase,
                pointer_id: value.pointer_id.get(),
                x: value.x.get(),
                y: value.y.get(),
                coordinate_width: value.coordinate_width.get(),
                coordinate_height: value.coordinate_height.get(),
                button: value.button.get(),
                buttons: value.buttons.get(),
                wheel_horizontal: value.wheel_horizontal.get(),
                wheel_vertical: value.wheel_vertical.get(),
                timestamp: value.timestamp.get(),
                sequence: value.sequence.get(),
            }))
        }
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

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_messages_round_trip() {
        let messages = [
            Message::Hello(Handshake {
                protocol_version: 1,
                process_id: 42,
                preferred_consumer: snowglass_protocol::ConsumerApi::D3d12,
            }),
            Message::StreamReady(StreamReady {
                producer_process_id: 7,
                surface_id: SurfaceId(0x1234),
                adapter_luid: AdapterLuid {
                    low_part: 0x1234,
                    high_part: 0x5678,
                },
                source_window: 8,
                width: 1920,
                height: 1080,
                format: 44,
                image_kind: ExternalImageKind::VulkanOpaqueWin32,
                sync_kind: ExternalSyncKind::VulkanTimelineSemaphore,
                slot_count: 3,
                image_handles: [11, 12, 13, 0],
                timeline_semaphore_handle: 14,
                consumer_semaphore_handle: 15,
                bottom_up: true,
            }),
            Message::FrameReady(FrameReady {
                stream_id: StreamId(9),
                slot: 2,
                render_width: 640,
                render_height: 528,
                capture_x: 10,
                capture_y: 20,
                capture_width: 620,
                capture_height: 488,
                presentation_width: 1920,
                presentation_height: 1080,
                presentation_transform: PresentationTransform::affine(
                    PresentationPoint::from_pixels(498, 20),
                    PresentationPoint::from_pixels(498, 640),
                    PresentationPoint::from_pixels(10, 20),
                ),
                frame_number: 100,
                sync_value: 101,
                timestamp_qpc: 102,
            }),
            Message::Shutdown,
            Message::DirectInputData(DirectInputData {
                device: DirectInputDevice::Keyboard,
                offset: 30,
                data: 0x80,
                timestamp: 123,
                sequence: 456,
            }),
            Message::PointerInput(PointerInput {
                surface_id: 0x1234,
                target_window: 8,
                frame_number: 100,
                source: PointerSource::Touch,
                phase: PointerPhase::ButtonDown,
                pointer_id: 7,
                x: 320,
                y: 240,
                coordinate_width: 1920,
                coordinate_height: 1080,
                button: 1,
                buttons: 1,
                wheel_horizontal: 0,
                wheel_vertical: 0,
                timestamp: 123,
                sequence: 457,
            }),
            Message::StreamAccepted,
        ];
        for message in messages {
            assert_eq!(decode(&encode(message)).unwrap(), message);
        }
    }

    #[test]
    fn af_unix_stream_round_trip() {
        let name = format!("snowglass-test-{}", std::process::id());
        let listener = LocalSocketListener::bind(&name).unwrap();
        let server = std::thread::spawn(move || {
            let mut socket = listener.accept().unwrap();
            assert_eq!(
                socket.receive().unwrap(),
                Message::Hello(Handshake {
                    protocol_version: 1,
                    process_id: 42,
                    preferred_consumer: snowglass_protocol::ConsumerApi::Vulkan,
                })
            );
            socket.send(Message::Shutdown).unwrap();
        });
        let mut client = LocalSocket::connect(&name).unwrap();
        client
            .send(Message::Hello(Handshake {
                protocol_version: 1,
                process_id: 42,
                preferred_consumer: snowglass_protocol::ConsumerApi::Vulkan,
            }))
            .unwrap();
        assert_eq!(client.receive().unwrap(), Message::Shutdown);
        server.join().unwrap();
    }

    #[test]
    fn listener_accepts_replacement_connection() {
        let name = format!("snowglass-reconnect-test-{}", std::process::id());
        let listener = LocalSocketListener::bind(&name).unwrap();
        let server = std::thread::spawn(move || {
            for expected_process_id in [41, 42] {
                let mut socket = listener.accept().unwrap();
                assert_eq!(
                    socket.receive().unwrap(),
                    Message::Hello(Handshake {
                        protocol_version: 1,
                        process_id: expected_process_id,
                        preferred_consumer: snowglass_protocol::ConsumerApi::Vulkan,
                    })
                );
                socket.send(Message::Shutdown).unwrap();
            }
        });
        for process_id in [41, 42] {
            let mut client = LocalSocket::connect(&name).unwrap();
            client
                .send(Message::Hello(Handshake {
                    protocol_version: 1,
                    process_id,
                    preferred_consumer: snowglass_protocol::ConsumerApi::Vulkan,
                }))
                .unwrap();
            assert_eq!(client.receive().unwrap(), Message::Shutdown);
        }
        server.join().unwrap();
    }
}
