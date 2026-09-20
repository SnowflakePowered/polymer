//! Control messages shared by the platform-local transports.
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;
mod wire;
#[cfg(windows)]
pub use windows::LocalSocketListener;
#[cfg(windows)]
use windows::Stream;

#[cfg(target_os = "macos")]
pub use macos::{LocalSocket, LocalSocketListener};
use polymer_protocol::{
    AdapterLuid, ExternalImageKind, ExternalSyncKind, FrameReady, Handshake, MAX_IMAGE_SLOTS,
    PresentationMappingKind, PresentationPoint, PresentationTransform, ResizeWindow, StreamId,
    SurfaceId,
};
use std::{io, mem, time::Duration};
#[cfg(windows)]
use std::{
    io::{Read, Write},
    net::Shutdown,
};
use wire::{WireMessage, decode, encode, invalid_data};
pub(crate) use wire::{decode_frame, encode_frame};
use zerocopy::{FromBytes, IntoBytes};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireFeedback {
    pub player: u8,
    pub low_frequency: u16,
    pub high_frequency: u16,
    pub duration_ms: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum BlobKind {
    Topology = 1,
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(1);
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
    ResizeWindow(ResizeWindow),
    GamepadFeedback(WireFeedback),
    Blob {
        kind: BlobKind,
        len: u32,
    },
    /// The viewer has imported the stream resources and released its previous stream.
    StreamAccepted,
    /// Subscribe before publishing a virtual stream. Ordinary capture streams
    /// do not need acknowledgments and must not accumulate unread responses.
    RequestBufferAvailability,
    /// The consumer has submitted release of every publication through this
    /// value. Rendering also requires completion of the shared release fence
    /// before reusing an image. The connection identifies
    /// the immutable resource generation; values never cross connections.
    BufferAvailable {
        stream_id: StreamId,
        through_sync_value: u64,
    },
    /// A file handle already duplicated into the receiving process.
    MapOffer(MapOffer),
    FrameAvailable,
    MapAccepted,
}

#[cfg(windows)]
pub struct LocalSocket {
    stream: Stream,
    frame: Option<crate::frame::FramePublisher>,
}
#[cfg(windows)]
impl LocalSocket {
    pub fn connect(name: &str) -> io::Result<Self> {
        Self::connect_timeout(name, CONNECT_TIMEOUT)
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
            frame: None,
        })
    }

    pub fn shutdown(&self) -> io::Result<()> {
        self.stream.shutdown(Shutdown::Both)
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

    pub fn send_blob(&mut self, kind: BlobKind, bytes: &[u8]) -> io::Result<()> {
        let len = u32::try_from(bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "blob is too large"))?;
        self.send(Message::Blob { kind, len })?;
        self.stream.write_all(bytes)
    }

    pub fn receive_blob(&mut self, len: u32) -> io::Result<Vec<u8>> {
        let mut bytes = vec![0; len as usize];
        self.stream.read_exact(&mut bytes)?;
        Ok(bytes)
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
