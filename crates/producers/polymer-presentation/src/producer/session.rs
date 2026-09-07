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
    last_publication: Option<(polymer_protocol::StreamId, u64)>,
    available_through: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use polymer_protocol::StreamId;
    use std::time::Instant;

    #[test]
    fn availability_rejects_foreign_and_future_publications_without_regression() {
        for (case, invalid) in [
            Message::BufferAvailable {
                stream_id: StreamId(10),
                through_sync_value: 3,
            },
            Message::BufferAvailable {
                stream_id: StreamId(9),
                through_sync_value: 8,
            },
        ]
        .into_iter()
        .enumerate()
        {
            let name = format!("polymer-availability-{}-{case}", std::process::id());
            let listener = polymer_ipc::LocalSocketListener::bind(&name).unwrap();
            let socket = LocalSocket::connect(&name).unwrap();
            let mut peer = listener.accept().unwrap();
            let mut session = ProducerSession {
                socket,
                viewer_pid: 0,
                preferred_consumer: ConsumerApi::D3d12,
                accepted: true,
                frame_number: 7,
                last_publication: Some((StreamId(9), 7)),
                available_through: 0,
            };
            for value in [3, 2, 3] {
                peer.send(Message::BufferAvailable {
                    stream_id: StreamId(9),
                    through_sync_value: value,
                })
                .unwrap();
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while session.poll_buffer_availability().unwrap() != 3 {
                assert!(Instant::now() < deadline, "availability stalled");
                std::thread::yield_now();
            }
            peer.send(invalid).unwrap();
            loop {
                match session.poll_buffer_availability() {
                    Err(error) => {
                        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                        assert_eq!(session.available_through, 3);
                        break;
                    }
                    Ok(value) => assert_eq!(value, 3),
                }
                assert!(
                    Instant::now() < deadline,
                    "invalid acknowledgment was accepted"
                );
                std::thread::yield_now();
            }
        }
    }
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
            last_publication: None,
            available_through: 0,
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

    pub fn request_buffer_availability(&mut self) -> io::Result<()> {
        self.socket.send(Message::RequestBufferAvailability)
    }

    /// Acknowledgments authorize reuse only within this accepted connection.
    /// The backend must additionally establish native GPU ownership.
    pub fn poll_buffer_availability(&mut self) -> io::Result<u64> {
        while let Some(message) = self.socket.try_receive()? {
            let Message::BufferAvailable {
                stream_id,
                through_sync_value,
            } = message
            else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected buffer acknowledgment",
                ));
            };
            if !self
                .last_publication
                .is_some_and(|(stream, value)| stream == stream_id && through_sync_value <= value)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "buffer acknowledgment belongs to an unknown publication",
                ));
            }
            self.available_through = self.available_through.max(through_sync_value);
        }
        Ok(self.available_through)
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

    /// Publish to an optional stream and release a failed connection.
    pub fn publish_frame(
        connection: &mut Option<Self>,
        frame: polymer_protocol::FrameReady,
    ) -> bool {
        let Some(session) = connection else {
            return false;
        };
        match session.send(frame) {
            Ok(()) => true,
            Err(error) => {
                tracing::debug!("polymer: viewer released capture stream: {error}");
                *connection = None;
                false
            }
        }
    }

    pub fn send(&mut self, mut frame: polymer_protocol::FrameReady) -> io::Result<()> {
        if !self.accepted {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "stream has not been accepted",
            ));
        }
        frame.frame_number = self.frame_number.wrapping_add(1);
        let diagnostic = (
            frame.stream_id,
            frame.slot,
            frame.render_width,
            frame.render_height,
            frame.sync_value,
        );
        match self.socket.send(Message::FrameReady(frame)) {
            Ok(()) => {
                self.last_publication = Some((diagnostic.0, diagnostic.4));
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
