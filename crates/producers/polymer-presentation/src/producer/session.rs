use super::{ExportDescriptor, SessionFailure, export::Export};
use polymer_ipc::{LocalSocket, Message};
use polymer_protocol::{ConsumerApi, PROTOCOL_VERSION};
use std::{io, time::Duration};

/// A single independently accepted capture stream.
pub struct ProducerSession {
    socket: LocalSocket,
    viewer_pid: u32,
    preferred_consumer: ConsumerApi,
    accepted: bool,
    frame_number: u64,
}

impl ProducerSession {
    /// Rejection and protocol incompatibility park this stream. Transport and
    /// export failures remain errors so the lifecycle applies transient backoff.
    pub fn setup_result<T>(result: io::Result<T>) -> io::Result<Option<T>> {
        match result {
            Ok(value) => Ok(Some(value)),
            Err(error) if SessionFailure::classify(&error) != SessionFailure::Connection => {
                tracing::debug!("polymer: capture session parked: {error}");
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub fn connect(name: &str) -> io::Result<Self> {
        let mut socket = LocalSocket::connect_timeout(name, Duration::ZERO)?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        socket.set_write_timeout(Some(Duration::from_secs(2)))?;
        let Message::Hello(hello) = socket.receive()? else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                SessionFailure::IncompatibleProtocol,
            ));
        };
        if hello.protocol_version != PROTOCOL_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                SessionFailure::IncompatibleProtocol,
            ));
        }
        socket.enable_frame_channel(hello.process_id)?;
        Ok(Self {
            socket,
            viewer_pid: hello.process_id,
            preferred_consumer: hello.preferred_consumer,
            accepted: false,
            frame_number: 0,
        })
    }

    pub fn viewer_pid(&self) -> u32 {
        self.viewer_pid
    }
    /// A busy ring still polls disconnects before attempting GPU work.
    pub fn connected(&self) -> bool {
        self.accepted && !self.socket.peer_disconnected().unwrap_or(true)
    }
    pub fn preferred_consumer(&self) -> ConsumerApi {
        self.preferred_consumer
    }

    /// Input handles are borrowed producer-local handles. The receiver owns
    /// duplicated handles once publication starts; a partial socket write must
    /// not race import by closing them from this process.
    pub fn publish(&mut self, descriptor: ExportDescriptor<'_>) -> io::Result<()> {
        if self.accepted {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "stream already published",
            ));
        }
        let mut ready = descriptor.into_ready()?;
        let mut export = Export::prepare(self.viewer_pid, &mut ready)?;
        export.transfer();
        self.socket.send(Message::StreamReady(ready))?;
        // The existing wire protocol declines a stream by closing after
        // StreamReady without acceptance. A timeout/reset remains retryable.
        let response = self.socket.receive().map_err(|error| {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                io::Error::new(io::ErrorKind::PermissionDenied, SessionFailure::Rejected)
            } else {
                error
            }
        })?;
        match response {
            Message::StreamAccepted => {
                self.accepted = true;
                Ok(())
            }
            _ => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                SessionFailure::Rejected,
            )),
        }
    }

    pub fn send(&mut self, message: Message) -> io::Result<()> {
        if !self.accepted {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "stream has not been accepted",
            ));
        }
        let frame = match message {
            Message::FrameReady(mut frame) => {
                frame.frame_number = self.frame_number.wrapping_add(1);
                frame
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "producer session expects a captured frame",
                ));
            }
        };
        let diagnostic = (
            frame.stream_id,
            frame.slot,
            frame.render_width,
            frame.render_height,
            frame.sync_value,
        );
        match self.socket.send(Message::FrameReady(frame)) {
            Ok(()) => {
                self.frame_number = self.frame_number.wrapping_add(1);
                if self.frame_number == 1 || self.frame_number.is_multiple_of(120) {
                    tracing::debug!(stream = ?diagnostic.0, slot = diagnostic.1,
                        width = diagnostic.2, height = diagnostic.3, sync_value = diagnostic.4,
                        frame = self.frame_number, "polymer: published capture frame");
                }
                Ok(())
            }
            Err(error) => {
                self.accepted = false;
                Err(error)
            }
        }
    }
}
