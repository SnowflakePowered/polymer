//! Capture-producer connection lifecycle shared by native hosts.

use snowglass_ipc::{LocalSocketListener, Message};
use snowglass_protocol::{ConsumerApi, FrameReady, Handshake, PROTOCOL_VERSION};
use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsumerPreference {
    D3d12,
    Vulkan,
}

pub enum CaptureEvent {
    Stream(u64, snowglass_ipc::StreamReady, mpsc::SyncSender<bool>),
    Frame(u64, FrameReady),
    Disconnected(u64),
}

pub struct Capture {
    events: mpsc::Receiver<CaptureEvent>,
    revoked: Arc<Mutex<HashSet<u64>>>,
}

impl Capture {
    pub fn bind(session: &str, preference: ConsumerPreference) -> std::io::Result<Self> {
        Self::bind_with_waker(session, preference, || {})
    }

    pub fn bind_with_waker(
        session: &str,
        preference: ConsumerPreference,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let listener = LocalSocketListener::bind(session)?;
        let (events, receiver) = mpsc::channel();
        let revoked = Arc::new(Mutex::new(HashSet::new()));
        spawn(
            listener,
            events,
            preference,
            revoked.clone(),
            Arc::new(wake),
        );
        Ok(Self {
            events: receiver,
            revoked,
        })
    }

    pub fn try_recv(&self) -> Result<CaptureEvent, mpsc::TryRecvError> {
        self.events.try_recv()
    }

    pub fn revoke(&self, connection: u64) {
        self.revoked
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(connection);
    }

    pub fn forget(&self, connection: u64) {
        self.revoked
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&connection);
    }
}

fn spawn(
    listener: LocalSocketListener,
    events: mpsc::Sender<CaptureEvent>,
    preference: ConsumerPreference,
    revoked: Arc<Mutex<HashSet<u64>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    thread::spawn(move || {
        let next_connection = Arc::new(AtomicU64::new(0));
        loop {
            let mut socket = match listener.accept() {
                Ok(socket) => socket,
                Err(error) => {
                    eprintln!("snowglass: capture IPC accept ended: {error}");
                    return;
                }
            };
            let connection = next_connection.fetch_add(1, Ordering::AcqRel) + 1;
            let preferred_consumer = match preference {
                ConsumerPreference::D3d12 => ConsumerApi::D3d12,
                ConsumerPreference::Vulkan => ConsumerApi::Vulkan,
            };
            if let Err(error) = socket.send(Message::Hello(Handshake {
                protocol_version: PROTOCOL_VERSION,
                process_id: std::process::id(),
                preferred_consumer,
            })) {
                eprintln!("snowglass: capture IPC handshake failed: {error}");
                continue;
            }
            let events = events.clone();
            let revoked = revoked.clone();
            let wake = wake.clone();
            thread::spawn(move || {
                let result = (|| -> std::io::Result<()> {
                    let mut accepted = false;
                    let mut frames = None;
                    loop {
                        let message = socket.receive()?;
                        if revoked
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .contains(&connection)
                        {
                            return Ok(());
                        }
                        match message {
                            Message::StreamReady(stream) => {
                                let (accepted_tx, accepted_rx) = mpsc::sync_channel(0);
                                events
                                    .send(CaptureEvent::Stream(connection, stream, accepted_tx))
                                    .map_err(|_| std::io::Error::other("capture host closed"))?;
                                wake();
                                accepted = accepted_rx.recv().unwrap_or(false);
                                if accepted {
                                    socket.send(Message::StreamAccepted)?;
                                } else {
                                    return Ok(());
                                }
                            }
                            Message::FrameReady(frame) if accepted => {
                                let _ = events.send(CaptureEvent::Frame(connection, frame));
                                wake();
                            }
                            Message::FrameAvailable if accepted => {
                                if let Some(frame) = frames.as_mut().and_then(
                                    |frames: &mut snowglass_ipc::frame::FrameSubscriber| {
                                        frames.latest().ok().flatten()
                                    },
                                ) {
                                    let _ = events.send(CaptureEvent::Frame(connection, frame));
                                    wake();
                                }
                            }
                            Message::Shutdown => return Ok(()),
                            Message::MapOffer(offer)
                                if offer.channel == snowglass_ipc::control::ChannelKind::Frame =>
                            {
                                frames =
                                    Some(snowglass_ipc::frame::FrameSubscriber::from_offer(offer)?);
                            }
                            _ => {}
                        }
                    }
                })();
                if let Err(error) = result
                    && error.kind() != std::io::ErrorKind::UnexpectedEof
                    && error.kind() != std::io::ErrorKind::ConnectionReset
                {
                    eprintln!("snowglass: capture IPC connection ended: {error}");
                }
                let _ = events.send(CaptureEvent::Disconnected(connection));
                wake();
            });
        }
    });
}
