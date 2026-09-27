#[cfg(windows)]
use super::export::Export;
use super::{ExportDescriptor, SessionFailure};
use parking_lot::{Condvar, Mutex};
use polymer_ipc::{LocalSocket, Message};
use polymer_protocol::{ConsumerApi, PROTOCOL_VERSION};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

#[derive(Clone, Default)]
pub struct AvailabilityWaiter {
    shared: Arc<Availability>,
}
impl AvailabilityWaiter {
    pub fn epoch(&self) -> u64 {
        self.shared.epoch.load(Ordering::Acquire)
    }

    pub fn wait(&self, observed: u64, timeout: Option<Duration>) {
        if self.epoch() != observed {
            return;
        }
        let mut guard = self.shared.wait.lock();
        if self.epoch() != observed {
            return;
        }
        match timeout {
            Some(timeout) => {
                self.shared.changed.wait_for(&mut guard, timeout);
            }
            None => self.shared.changed.wait(&mut guard),
        }
    }

    pub fn notify(&self) {
        self.shared.notify();
    }
}

struct Availability {
    state: Mutex<AvailabilityState>,
    epoch: AtomicU64,
    wait: Mutex<()>,
    changed: Condvar,
}
impl Default for Availability {
    fn default() -> Self {
        Self {
            state: Mutex::new(AvailabilityState::default()),
            epoch: AtomicU64::new(0),
            wait: Mutex::new(()),
            changed: Condvar::new(),
        }
    }
}
impl Availability {
    fn notify(&self) {
        let _guard = self.wait.lock();
        self.epoch.fetch_add(1, Ordering::Release);
        self.changed.notify_all();
    }
}

#[derive(Default)]
struct AvailabilityState {
    last_publication: Option<(polymer_protocol::StreamId, u64)>,
    available_through: u64,
    error: Option<(io::ErrorKind, String)>,
}

/// A single independently accepted capture stream.
pub struct ProducerSession {
    socket: LocalSocket,
    viewer_pid: u32,
    preferred_consumer: ConsumerApi,
    accepted: bool,
    frame_number: u64,
    availability: Arc<Availability>,
    receiver: Option<JoinHandle<()>>,
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
            availability: Arc::new(Availability::default()),
            receiver: None,
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

    pub fn availability(&self) -> AvailabilityWaiter {
        AvailabilityWaiter {
            shared: self.availability.clone(),
        }
    }

    pub fn request_buffer_availability(&mut self) -> io::Result<()> {
        self.socket.send(Message::RequestBufferAvailability)
    }

    /// Acknowledgments authorize reuse only within this accepted connection.
    /// The backend must additionally establish native GPU ownership.
    pub fn poll_buffer_availability(&mut self) -> io::Result<u64> {
        let state = self.availability.state.lock();
        if let Some((kind, message)) = &state.error {
            return Err(io::Error::new(*kind, message.clone()));
        }
        Ok(state.available_through)
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
        #[cfg(target_os = "macos")]
        let resources = descriptor.resources.clone();
        let ready = descriptor.into_ready()?;
        #[cfg(windows)]
        let mut ready = ready;
        #[cfg(windows)]
        let mut export = Export::prepare(self.viewer_pid, &mut ready)?;
        #[cfg(windows)]
        export.transfer();
        #[cfg(windows)]
        self.socket.send(Message::StreamReady(ready))?;
        #[cfg(target_os = "macos")]
        self.socket.send_stream(ready, resources)?;
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
                self.start_receiver()?;
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
        self.availability.state.lock().last_publication = Some((diagnostic.0, diagnostic.4));
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

    fn start_receiver(&mut self) -> io::Result<()> {
        if self.receiver.is_some() {
            return Ok(());
        }
        let mut socket = self.socket.try_clone()?;
        socket.set_read_timeout(None)?;
        let availability = self.availability.clone();
        self.receiver = Some(
            std::thread::Builder::new()
                .name("polymer-buffer-availability".into())
                .spawn(move || {
                    loop {
                        let result = socket.receive().and_then(|message| {
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
                            let mut state = availability.state.lock();
                            if !state.last_publication.is_some_and(|(stream, value)| {
                                stream == stream_id && through_sync_value <= value
                            }) {
                                return Err(io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "buffer acknowledgment belongs to an unknown publication",
                                ));
                            }
                            state.available_through =
                                state.available_through.max(through_sync_value);
                            Ok(())
                        });
                        match result {
                            Ok(()) => availability.notify(),
                            Err(error) => {
                                availability.state.lock().error =
                                    Some((error.kind(), error.to_string()));
                                availability.notify();
                                return;
                            }
                        }
                    }
                })?,
        );
        Ok(())
    }
}

impl Drop for ProducerSession {
    fn drop(&mut self) {
        let _ = self.socket.shutdown();
        if let Some(receiver) = self.receiver.take() {
            let _ = receiver.join();
        }
    }
}
