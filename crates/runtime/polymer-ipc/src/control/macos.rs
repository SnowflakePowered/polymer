use super::*;
use crate::xpc::{Connection, Listener, Packet};
use std::{
    fs::File,
    os::fd::{FromRawFd, IntoRawFd},
    thread,
    time::Instant,
};

/// Compatibility name for the common control API; macOS uses NSXPC, not sockets.
#[derive(Clone)]
pub struct LocalSocketListener(Listener);
impl LocalSocketListener {
    pub fn bind(name: &str) -> io::Result<Self> {
        Listener::bind(name).map(Self)
    }
    pub fn accept(&self) -> io::Result<LocalSocket> {
        self.0.accept().map(LocalSocket::new)
    }
    pub fn accept_timeout(&self, timeout: Duration) -> io::Result<LocalSocket> {
        self.0.accept_timeout(timeout).map(LocalSocket::new)
    }
    pub fn close(&self) {
        self.0.close();
    }
}
pub struct LocalSocket {
    connection: Connection,
    frame: Option<crate::frame::FramePublisher>,
    resources: Option<crate::xpc::MetalResources>,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}
impl LocalSocket {
    fn new(connection: Connection) -> Self {
        Self {
            connection,
            frame: None,
            resources: None,
            read_timeout: None,
            write_timeout: None,
        }
    }
    pub fn connect(name: &str) -> io::Result<Self> {
        Self::connect_timeout(name, CONNECT_TIMEOUT)
    }
    pub fn connect_timeout(name: &str, timeout: Duration) -> io::Result<Self> {
        let deadline = Instant::now() + timeout;
        loop {
            match Connection::connect(name) {
                Ok(connection) => return Ok(Self::new(connection)),
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound && Instant::now() < deadline =>
                {
                    thread::sleep(CONNECT_RETRY_DELAY)
                }
                Err(error) => return Err(error),
            }
        }
    }
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            connection: self.connection.clone(),
            frame: None,
            resources: None,
            read_timeout: self.read_timeout,
            write_timeout: self.write_timeout,
        })
    }
    pub fn peer_disconnected(&self) -> io::Result<bool> {
        Ok(self.connection.peer_disconnected())
    }
    pub fn shutdown(&self) -> io::Result<()> {
        self.connection.close();
        Ok(())
    }
    pub fn send(&mut self, mut message: Message) -> io::Result<()> {
        if let (Some(frame), Message::FrameReady(value)) = (&mut self.frame, &message) {
            frame.publish(*value)?;
            message = Message::FrameAvailable;
        }
        let file = if let Message::MapOffer(ref mut offer) = message {
            let descriptor = i32::try_from(offer.handle)
                .map_err(|_| invalid_data("invalid mapping descriptor"))?;
            let duplicate = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, 0) };
            if duplicate < 0 {
                return Err(io::Error::last_os_error());
            }
            offer.handle = 0; // Only NSFileHandle transfers authority across processes.
            Some(unsafe { File::from_raw_fd(duplicate) })
        } else {
            None
        };
        let mut data = encode(&message)?.as_bytes().to_vec();
        if let Message::Blob { data: payload, .. } = message {
            data.extend_from_slice(&payload);
        }
        self.connection.send(
            Packet {
                data,
                file,
                ..Default::default()
            },
            self.write_timeout,
        )
    }
    fn decode(&mut self, packet: Packet) -> io::Result<Message> {
        let (wire, data) = WireMessage::ref_from_prefix(&packet.data)
            .map_err(|_| invalid_data("invalid control packet"))?;
        let mut message = decode(wire, data.to_vec())?;
        if let Message::StreamReady(ready) = &message {
            if ready.image_kind != ExternalImageKind::IOSurface
                || ready.sync_kind != ExternalSyncKind::MetalSharedEvent
                || packet.surfaces.len() != ready.slot_count as usize
                || packet.surfaces.is_empty()
            {
                return Err(invalid_data("invalid Metal stream resources"));
            }
            self.resources = Some(crate::xpc::MetalResources {
                surfaces: packet.surfaces,
                ready: packet
                    .event
                    .ok_or_else(|| invalid_data("missing producer event"))?,
                released: packet
                    .consumer_event
                    .ok_or_else(|| invalid_data("missing consumer event"))?,
            });
        } else if !packet.surfaces.is_empty()
            || packet.event.is_some()
            || packet.consumer_event.is_some()
        {
            return Err(invalid_data(
                "unexpected native resources in control packet",
            ));
        }
        match (&mut message, packet.file) {
            (Message::MapOffer(offer), Some(file)) => offer.handle = i64::from(file.into_raw_fd()),
            (Message::MapOffer(_), None) | (_, Some(_)) => {
                return Err(invalid_data("unexpected mapping attachment"));
            }
            _ => {}
        }
        Ok(message)
    }
    pub fn send_stream(
        &mut self,
        ready: StreamReady,
        resources: crate::xpc::MetalResources,
    ) -> io::Result<()> {
        self.connection.send(
            Packet {
                data: encode(&Message::StreamReady(ready))?.as_bytes().to_vec(),
                surfaces: resources.surfaces,
                event: Some(resources.ready),
                consumer_event: Some(resources.released),
                ..Default::default()
            },
            self.write_timeout,
        )
    }
    pub fn take_stream_resources(&mut self) -> Option<crate::xpc::MetalResources> {
        self.resources.take()
    }
    pub fn receive(&mut self) -> io::Result<Message> {
        self.decode(self.connection.receive(self.read_timeout)?)
    }
    pub fn try_receive(&mut self) -> io::Result<Option<Message>> {
        match self.connection.receive(Some(Duration::ZERO)) {
            Ok(packet) => self.decode(packet).map(Some),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => Ok(None),
            Err(error) => Err(error),
        }
    }
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.read_timeout = timeout;
        Ok(())
    }
    pub fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.write_timeout = timeout;
        Ok(())
    }
    pub fn enable_frame_channel(&mut self, peer_process_id: u32) -> io::Result<()> {
        let publisher = crate::frame::FramePublisher::new()?;
        publisher.offer(self, peer_process_id)?;
        self.frame = Some(publisher);
        Ok(())
    }
}
