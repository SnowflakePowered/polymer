use super::ViewerApi;
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

pub(crate) enum CaptureEvent {
    Stream(u64, snowglass_ipc::StreamReady, mpsc::SyncSender<bool>),
    Frame(u64, FrameReady),
    Disconnected(u64),
}

pub(crate) fn spawn(
    listener: LocalSocketListener,
    events: mpsc::Sender<CaptureEvent>,
    viewer_api: ViewerApi,
    revoked: Arc<Mutex<HashSet<u64>>>,
) {
    thread::spawn(move || {
        let next_connection = Arc::new(AtomicU64::new(0));
        loop {
            let mut socket = match listener.accept() {
                Ok(socket) => socket,
                Err(error) => {
                    eprintln!("snowglass: IPC accept ended: {error}");
                    return;
                }
            };
            let connection = next_connection.fetch_add(1, Ordering::AcqRel) + 1;
            if let Err(error) = socket.send(Message::Hello(Handshake {
                protocol_version: PROTOCOL_VERSION,
                process_id: std::process::id(),
                preferred_consumer: match viewer_api {
                    ViewerApi::D3d12 => ConsumerApi::D3d12,
                    ViewerApi::Vulkan => ConsumerApi::Vulkan,
                },
            })) {
                eprintln!("snowglass: IPC handshake failed: {error}");
                continue;
            }
            let events = events.clone();
            let revoked = revoked.clone();
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
                                    .map_err(|_| {
                                        std::io::Error::new(
                                            std::io::ErrorKind::BrokenPipe,
                                            "viewer closed",
                                        )
                                    })?;
                                accepted = accepted_rx.recv().unwrap_or(false);
                                if accepted {
                                    socket.send(Message::StreamAccepted)?;
                                } else {
                                    return Ok(());
                                }
                            }
                            Message::FrameReady(frame) => {
                                if accepted {
                                    let _ = events.send(CaptureEvent::Frame(connection, frame));
                                }
                            }
                            Message::FrameAvailable => {
                                if accepted
                                    && let Some(frame) = frames.as_mut().and_then(
                                        |frames: &mut snowglass_ipc::frame::FrameSubscriber| {
                                            frames.latest().ok().flatten()
                                        },
                                    )
                                {
                                    let _ = events.send(CaptureEvent::Frame(connection, frame));
                                }
                            }
                            Message::Shutdown => return Ok(()),
                            Message::MapOffer(offer)
                                if offer.channel == snowglass_ipc::control::ChannelKind::Frame =>
                            {
                                frames =
                                    Some(snowglass_ipc::frame::FrameSubscriber::from_offer(offer)?);
                            }
                            Message::DirectInputData(_)
                            | Message::RawKeyInput(_)
                            | Message::PointerInput(_)
                            | Message::GamepadUpdate(_)
                            | Message::Hello(_)
                            | Message::StreamAccepted
                            | Message::MapOffer(_)
                            | Message::MapAccepted => {}
                        }
                    }
                })();
                if let Err(error) = result
                    && error.kind() != std::io::ErrorKind::UnexpectedEof
                    && error.kind() != std::io::ErrorKind::ConnectionReset
                {
                    eprintln!("snowglass: IPC connection ended: {error}");
                }
                let _ = events.send(CaptureEvent::Disconnected(connection));
            });
        }
    });
}
