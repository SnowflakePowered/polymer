//! Control messages shared by the platform-local transports.
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;
mod wire;
#[cfg(windows)]
use windows::Stream;
#[cfg(windows)]
pub use windows::{LocalSocketListener, Win32Resources};

#[cfg(target_os = "macos")]
pub use macos::{LocalSocket, LocalSocketListener};
use polymer_protocol::{
    AdapterLuid, ExternalImageKind, GraphicsHook, Handshake, MAX_IMAGE_SLOTS, SurfaceId,
};
use std::{io, mem, time::Duration};
#[cfg(windows)]
use std::{
    io::{Read, Write},
    net::Shutdown,
};
use wire::{WireMessage, decode, encode, invalid_data};
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
    /// Serialized CGEvent for process-scoped AppKit automation.
    MacosAutomationEvent = 2,
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(1);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamReady {
    /// Opaque logical surface identity. Unlike `source_window`, this may be
    /// synthesized to distinguish multiple frame-graph surfaces from one HWND.
    pub surface_id: SurfaceId,
    pub adapter_luid: AdapterLuid,
    /// Real Win32 HWND used as the target for pointer and keyboard messages.
    pub source_window: i64,
    pub hook: GraphicsHook,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub image_kind: ExternalImageKind,
    pub slot_count: u32,
    /// Handles already duplicated into the viewer process.
    pub image_handles: [i64; MAX_IMAGE_SLOTS],
    pub timeline_semaphore_handle: i64,
    /// Release timeline signalled by the viewer once it stops reading a slot.
    pub consumer_semaphore_handle: i64,
    /// Pixel rows are stored bottom-up (OpenGL default-framebuffer origin);
    /// the consumer flips vertically while presenting. Producers pass
    /// captures through verbatim and describe the orientation instead.
    pub bottom_up: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MapOffer {
    pub handle: i64,
    pub mapping_len: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Hello(Handshake),
    StreamReady(StreamReady),
    GamepadFeedback(WireFeedback),
    Blob {
        kind: BlobKind,
        data: Vec<u8>,
    },
    /// The viewer has imported the stream resources and released its previous stream.
    StreamAccepted,
    /// A file handle already duplicated into the receiving process.
    MapOffer(MapOffer),
    MapAccepted,
}

#[cfg(windows)]
pub struct LocalSocket {
    stream: Stream,
    incoming: Vec<u8>,
    resources: Option<Win32Resources>,
}
#[cfg(windows)]
impl LocalSocket {
    pub fn connect(name: &str) -> io::Result<Self> {
        Self::connect_timeout(name, CONNECT_TIMEOUT)
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
            incoming: Vec::new(),
            resources: None,
        })
    }

    pub fn shutdown(&self) -> io::Result<()> {
        self.stream.shutdown(Shutdown::Both)
    }

    pub fn send(&mut self, message: Message) -> io::Result<()> {
        self.stream.write_all(encode(&message)?.as_bytes())?;
        if let Message::Blob { data, .. } = message {
            self.stream.write_all(&data)?;
        }
        Ok(())
    }

    pub fn receive(&mut self) -> io::Result<Message> {
        let header_len = mem::size_of::<WireMessage>();
        self.read_control_until(header_len)?;
        let wire = WireMessage::ref_from_bytes(&self.incoming[..header_len])
            .map_err(|_| invalid_data("invalid wire message layout"))?;
        self.read_control_until(header_len + wire.data_len()?)?;
        let bytes = mem::take(&mut self.incoming);
        let (wire, data) = WireMessage::ref_from_prefix(&bytes)
            .map_err(|_| invalid_data("invalid wire message layout"))?;
        let message = decode(wire, data.to_vec())?;
        if let Message::StreamReady(stream) = &message {
            self.resources = Some(Win32Resources::take(stream)?);
        }
        Ok(message)
    }

    // Preserve partial reads across timeouts and polling calls, including blobs
    // larger than the socket receive buffer.
    fn read_control_until(&mut self, length: usize) -> io::Result<()> {
        let mut buffer = [0; 8192];
        while self.incoming.len() < length {
            let count = buffer.len().min(length - self.incoming.len());
            let read = self.stream.read(&mut buffer[..count])?;
            if read == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            self.incoming.extend_from_slice(&buffer[..read]);
        }
        Ok(())
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.set_write_timeout(timeout)
    }
}
