//! Snowglass control-plane IPC over a Windows AF_UNIX stream socket.

use snowglass_input::{
    DirectInputData, DirectInputDevice, RawKeyInput,
    gamepad::{
        GamepadFeedback, GamepadSlot, GamepadState, GamepadUpdate, VirtualGamepadConnection,
    },
    mouse::{PointerInput, PointerPhase, PointerSource},
};
use snowglass_protocol::{
    AdapterLuid, ExternalImageKind, ExternalSyncKind, FrameReady, Handshake, MAX_IMAGE_SLOTS,
    PresentationMappingKind, PresentationPoint, PresentationTransform, ResizeWindow, StreamId,
    SurfaceId,
};
use std::{
    fs,
    io::{self, Read, Write},
    mem,
    net::{Shutdown, TcpStream},
    os::windows::io::FromRawSocket,
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::Networking::WinSock::{
    AF_UNIX, INVALID_SOCKET, SOCK_STREAM, SOCKADDR, SOCKADDR_UN, SOCKET, SOMAXCONN, WSADATA,
    WSAGetLastError, WSAStartup, accept, bind, closesocket, connect, listen, socket,
};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::little_endian::{F32, I16, I32, I64, U16, U32, U64},
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
const MESSAGE_GAMEPAD_UPDATE: u32 = 8;
const MESSAGE_MAP_OFFER: u32 = 9;
const MESSAGE_FRAME_AVAILABLE: u32 = 10;
const MESSAGE_MAP_ACCEPTED: u32 = 11;
const MESSAGE_RAW_KEY_INPUT: u32 = 12;
const MESSAGE_RESIZE_WINDOW: u32 = 13;
const MESSAGE_GAMEPAD_FEEDBACK: u32 = 14;
const MESSAGE_VIRTUAL_GAMEPAD_CONNECTION: u32 = 15;
const PAYLOAD_SIZE: usize = payload_size();

const fn payload_size() -> usize {
    let stream = mem::size_of::<WireStreamReady>();
    let frame = mem::size_of::<WireFrameReady>();
    let pointer = mem::size_of::<WirePointerInput>();
    let gamepad = mem::size_of::<WireGamepadUpdate>();
    let map = mem::size_of::<WireMapOffer>();
    max(max(stream, frame), max(max(pointer, gamepad), map))
}

const fn max(left: usize, right: usize) -> usize {
    if left > right { left } else { right }
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
#[repr(u32)]
pub enum ChannelKind {
    Frame = 1,
    Input = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MapOffer {
    pub channel: ChannelKind,
    pub handle: i64,
    pub mapping_len: u64,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Message {
    Hello(Handshake),
    StreamReady(StreamReady),
    FrameReady(FrameReady),
    Shutdown,
    DirectInputData(DirectInputData),
    RawKeyInput(RawKeyInput),
    ResizeWindow(ResizeWindow),
    PointerInput(PointerInput),
    GamepadUpdate(GamepadUpdate),
    GamepadFeedback(GamepadFeedback),
    VirtualGamepadConnection(VirtualGamepadConnection),
    /// The viewer has imported the stream resources and released its previous stream.
    StreamAccepted,
    /// A file handle already duplicated into the receiving process.
    MapOffer(MapOffer),
    FrameAvailable,
    MapAccepted,
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
struct WireRawKeyInput {
    target_window: I64,
    virtual_key: U32,
    scan_code: U16,
    extended: U16,
    pressed: U16,
    repeat: U16,
    timestamp: U32,
    sequence: U32,
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

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct WireGamepadUpdate {
    slot: U32,
    sequence: U32,
    connected: U32,
    buttons: U32,
    left_x: I16,
    left_y: I16,
    right_x: I16,
    right_y: I16,
    left_trigger: U16,
    right_trigger: U16,
    pointer_x: I16,
    pointer_y: I16,
    accelerometer: [F32; 3],
    gyroscope: [F32; 3],
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct WireGamepadFeedback {
    slot: U32,
    low_frequency: U16,
    high_frequency: U16,
    duration_ms: U32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct WireVirtualGamepadConnection {
    slot: U32,
    connected: U32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct WireMapOffer {
    channel: U32,
    handle: I64,
    mapping_len: U64,
    generation: U64,
}

#[derive(Clone)]
pub struct LocalSocketListener {
    inner: Arc<ListenerInner>,
}

struct ListenerInner {
    socket: AtomicUsize,
    path: PathBuf,
    path_owned: AtomicBool,
}

// SAFETY: the listener exclusively owns its socket. Moving the socket between
// threads does not duplicate access or invalidate it.
unsafe impl Send for LocalSocketListener {}
// SAFETY: the socket value is atomic. Winsock permits closing a listening
// socket from another thread to interrupt a blocking accept.
unsafe impl Sync for LocalSocketListener {}

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
        Ok(Self {
            inner: Arc::new(ListenerInner {
                socket: AtomicUsize::new(socket),
                path,
                path_owned: AtomicBool::new(true),
            }),
        })
    }

    pub fn accept(&self) -> io::Result<LocalSocket> {
        let socket = self.inner.socket.load(Ordering::Acquire);
        if socket == INVALID_SOCKET {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "local socket listener is closed",
            ));
        }
        // SAFETY: self.socket is a listening socket. Peer address information is
        // not needed for this local, single-client transport.
        let connected = unsafe { accept(socket, std::ptr::null_mut(), std::ptr::null_mut()) };
        if connected == INVALID_SOCKET {
            return Err(winsock_error());
        }
        // SAFETY: ownership of the accepted Winsock socket moves into TcpStream.
        let stream = unsafe { TcpStream::from_raw_socket(connected as _) };
        Ok(LocalSocket {
            stream,
            frame: None,
        })
    }

    /// Stops this listener and wakes a thread blocked in [`Self::accept`].
    pub fn close(&self) {
        let socket = self.inner.socket.swap(INVALID_SOCKET, Ordering::AcqRel);
        if socket != INVALID_SOCKET {
            // SAFETY: the successful swap assigns this call sole responsibility
            // for closing the live socket.
            unsafe { closesocket(socket) };
        }
        // The acceptor thread owns another Arc while it unwinds. Unlink now so
        // a replacement emulator instance can bind the same session name
        // immediately after close returns.
        if self.inner.path_owned.swap(false, Ordering::AcqRel) {
            let _ = fs::remove_file(&self.inner.path);
        }
    }
}

impl Drop for ListenerInner {
    fn drop(&mut self) {
        let socket = *self.socket.get_mut();
        if socket != INVALID_SOCKET {
            // SAFETY: the final Arc owns the remaining live listening socket.
            unsafe { closesocket(socket) };
            *self.socket.get_mut() = INVALID_SOCKET;
        }
        if *self.path_owned.get_mut() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub struct LocalSocket {
    stream: TcpStream,
    frame: Option<crate::frame::FramePublisher>,
}

impl LocalSocket {
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
            frame: None,
        })
    }

    pub fn shutdown(&self) -> io::Result<()> {
        self.stream.shutdown(Shutdown::Both)
    }

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
                return Ok(Self {
                    stream,
                    frame: None,
                });
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
        if let (Some(frame), Message::FrameReady(value)) = (&mut self.frame, message) {
            frame.publish(value)?;
            return self
                .stream
                .write_all(encode(Message::FrameAvailable).as_bytes());
        }
        self.stream.write_all(encode(message).as_bytes())
    }

    pub fn enable_frame_channel(&mut self, peer_process_id: u32) -> io::Result<()> {
        let publisher = crate::frame::FramePublisher::new()?;
        publisher.offer(self, peer_process_id)?;
        self.frame = Some(publisher);
        Ok(())
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

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.set_write_timeout(timeout)
    }

    pub(crate) fn write_packet(&mut self, payload: &[u8]) -> io::Result<()> {
        let length = u32::try_from(payload.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "packet is too large"))?;
        self.stream.write_all(&length.to_le_bytes())?;
        self.stream.write_all(payload)
    }

    pub(crate) fn read_packet(&mut self, maximum: usize) -> io::Result<Vec<u8>> {
        let mut length = [0; 4];
        self.stream.read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if length > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "packet exceeds the configured maximum",
            ));
        }
        let mut payload = vec![0; length];
        self.stream.read_exact(&mut payload)?;
        Ok(payload)
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
        Message::RawKeyInput(value) => {
            set_payload(
                &mut wire,
                MESSAGE_RAW_KEY_INPUT,
                &WireRawKeyInput {
                    target_window: I64::new(value.target_window),
                    virtual_key: U32::new(value.virtual_key),
                    scan_code: U16::new(value.scan_code),
                    extended: U16::new(value.extended as u16),
                    pressed: U16::new(value.pressed as u16),
                    repeat: U16::new(value.repeat as u16),
                    timestamp: U32::new(value.timestamp),
                    sequence: U32::new(value.sequence),
                },
            );
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
        Message::GamepadUpdate(value) => {
            set_payload(
                &mut wire,
                MESSAGE_GAMEPAD_UPDATE,
                &WireGamepadUpdate {
                    slot: U32::new(u32::from(value.slot.get())),
                    sequence: U32::new(value.sequence),
                    connected: U32::new(value.connected as u32),
                    buttons: U32::new(value.state.buttons),
                    left_x: I16::new(value.state.left_stick[0]),
                    left_y: I16::new(value.state.left_stick[1]),
                    right_x: I16::new(value.state.right_stick[0]),
                    right_y: I16::new(value.state.right_stick[1]),
                    left_trigger: U16::new(value.state.left_trigger),
                    right_trigger: U16::new(value.state.right_trigger),
                    pointer_x: I16::new(value.state.pointer[0]),
                    pointer_y: I16::new(value.state.pointer[1]),
                    accelerometer: value.state.accelerometer.map(F32::new),
                    gyroscope: value.state.gyroscope.map(F32::new),
                },
            );
        }
        Message::GamepadFeedback(value) => {
            set_payload(
                &mut wire,
                MESSAGE_GAMEPAD_FEEDBACK,
                &WireGamepadFeedback {
                    slot: U32::new(u32::from(value.slot.get())),
                    low_frequency: U16::new(value.low_frequency),
                    high_frequency: U16::new(value.high_frequency),
                    duration_ms: U32::new(value.duration_ms),
                },
            );
        }
        Message::VirtualGamepadConnection(value) => {
            set_payload(
                &mut wire,
                MESSAGE_VIRTUAL_GAMEPAD_CONNECTION,
                &WireVirtualGamepadConnection {
                    slot: U32::new(u32::from(value.slot.get())),
                    connected: U32::new(value.connected as u32),
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
        Message::FrameAvailable => wire.kind = U32::new(MESSAGE_FRAME_AVAILABLE),
        Message::MapAccepted => wire.kind = U32::new(MESSAGE_MAP_ACCEPTED),
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
        MESSAGE_RAW_KEY_INPUT => {
            let value = payload::<WireRawKeyInput>(wire)?;
            let flag = |value: u16| match value {
                0 => Ok(false),
                1 => Ok(true),
                _ => Err(invalid_data("invalid raw-key boolean")),
            };
            Ok(Message::RawKeyInput(RawKeyInput {
                target_window: value.target_window.get(),
                virtual_key: value.virtual_key.get(),
                scan_code: value.scan_code.get(),
                extended: flag(value.extended.get())?,
                pressed: flag(value.pressed.get())?,
                repeat: flag(value.repeat.get())?,
                timestamp: value.timestamp.get(),
                sequence: value.sequence.get(),
            }))
        }
        MESSAGE_RESIZE_WINDOW => {
            let value = payload::<WireResizeWindow>(wire)?;
            Ok(Message::ResizeWindow(ResizeWindow {
                target_window: value.target_window.get(),
                width: value.width.get(),
                height: value.height.get(),
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
        MESSAGE_GAMEPAD_UPDATE => {
            let value = payload::<WireGamepadUpdate>(wire)?;
            let slot = u8::try_from(value.slot.get())
                .ok()
                .and_then(GamepadSlot::new)
                .ok_or_else(|| invalid_data("invalid gamepad slot"))?;
            let connected = match value.connected.get() {
                0 => false,
                1 => true,
                _ => return Err(invalid_data("invalid gamepad connection state")),
            };
            Ok(Message::GamepadUpdate(GamepadUpdate {
                slot,
                sequence: value.sequence.get(),
                connected,
                state: GamepadState {
                    buttons: value.buttons.get(),
                    left_stick: [value.left_x.get(), value.left_y.get()],
                    right_stick: [value.right_x.get(), value.right_y.get()],
                    left_trigger: value.left_trigger.get(),
                    right_trigger: value.right_trigger.get(),
                    pointer: [value.pointer_x.get(), value.pointer_y.get()],
                    accelerometer: value.accelerometer.map(F32::get),
                    gyroscope: value.gyroscope.map(F32::get),
                },
            }))
        }
        MESSAGE_GAMEPAD_FEEDBACK => {
            let value = payload::<WireGamepadFeedback>(wire)?;
            let slot = u8::try_from(value.slot.get())
                .ok()
                .and_then(GamepadSlot::new)
                .ok_or_else(|| invalid_data("invalid feedback gamepad slot"))?;
            Ok(Message::GamepadFeedback(GamepadFeedback {
                slot,
                low_frequency: value.low_frequency.get(),
                high_frequency: value.high_frequency.get(),
                duration_ms: value.duration_ms.get(),
            }))
        }
        MESSAGE_VIRTUAL_GAMEPAD_CONNECTION => {
            let value = payload::<WireVirtualGamepadConnection>(wire)?;
            let slot = u8::try_from(value.slot.get())
                .ok()
                .and_then(GamepadSlot::new)
                .ok_or_else(|| invalid_data("invalid virtual gamepad slot"))?;
            let connected = match value.connected.get() {
                0 => false,
                1 => true,
                _ => return Err(invalid_data("invalid virtual gamepad connection state")),
            };
            Ok(Message::VirtualGamepadConnection(
                VirtualGamepadConnection { slot, connected },
            ))
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

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(crate) fn encode_frame(value: FrameReady) -> Vec<u8> {
    let wire = encode(Message::FrameReady(value));
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
    match decode(&wire)? {
        Message::FrameReady(value) => Ok(value),
        _ => unreachable!(),
    }
}

pub(crate) fn encode_input(value: Message) -> Vec<u8> {
    debug_assert!(matches!(
        value,
        Message::GamepadUpdate(_) | Message::VirtualGamepadConnection(_)
    ));
    encode(value).as_bytes().to_vec()
}

pub(crate) fn decode_input(bytes: &[u8]) -> io::Result<Message> {
    let wire = WireMessage::read_from_bytes(bytes)
        .map_err(|_| invalid_data("invalid shared input event size"))?;
    match decode(&wire)? {
        event @ (Message::GamepadUpdate(_) | Message::VirtualGamepadConnection(_)) => Ok(event),
        _ => Err(invalid_data("invalid shared input event kind")),
    }
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
            Message::RawKeyInput(RawKeyInput {
                target_window: 8,
                virtual_key: 0x70,
                scan_code: 0x3b,
                extended: false,
                pressed: true,
                repeat: false,
                timestamp: 124,
                sequence: 457,
            }),
            Message::ResizeWindow(ResizeWindow {
                target_window: 8,
                width: 1280,
                height: 720,
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
            Message::GamepadUpdate(GamepadUpdate {
                slot: GamepadSlot::new(2).unwrap(),
                sequence: 458,
                connected: true,
                state: GamepadState {
                    buttons: 0x5,
                    left_stick: [-123, 456],
                    right_stick: [-789, 1_234],
                    left_trigger: 12_345,
                    right_trigger: 54_321,
                    pointer: [-20_000, 20_000],
                    accelerometer: [1.25, -2.5, 9.81],
                    gyroscope: [0.1, -0.2, 0.3],
                },
            }),
            Message::GamepadFeedback(GamepadFeedback {
                slot: GamepadSlot::new(2).unwrap(),
                low_frequency: 12_345,
                high_frequency: 54_321,
                duration_ms: 250,
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

    #[test]
    fn closed_listener_can_be_rebound_before_its_last_clone_drops() {
        let name = format!("snowglass-rebind-test-{}", std::process::id());
        let listener = LocalSocketListener::bind(&name).unwrap();
        let lingering_clone = listener.clone();
        listener.close();

        let replacement = LocalSocketListener::bind(&name).unwrap();
        drop(lingering_clone);
        let server = std::thread::spawn(move || {
            let mut socket = replacement.accept().unwrap();
            socket.send(Message::Shutdown).unwrap();
        });
        let mut client = LocalSocket::connect(&name).unwrap();
        assert_eq!(client.receive().unwrap(), Message::Shutdown);
        server.join().unwrap();
    }
}
