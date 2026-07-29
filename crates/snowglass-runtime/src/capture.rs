//! Target-to-viewer capture transport built on the existing Snowglass IPC.

use crossfire::oneshot;
use dashmap::{DashMap, DashSet};
use parking_lot::Mutex;
use snowglass_ipc::{LocalSocket, LocalSocketListener, Message};
use snowglass_protocol::{ConsumerApi, FrameReady, Handshake, PROTOCOL_VERSION};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
};
use watchable::Watchable;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConsumerPreference {
    D3d12,
    Vulkan,
}

#[derive(Clone)]
pub(crate) struct CaptureCallbacks {
    pub(crate) offer:
        Arc<dyn Fn(u64, snowglass_ipc::StreamReady, oneshot::TxOneshot<bool>) + Send + Sync>,
    pub(crate) disconnected: Arc<dyn Fn(u64) + Send + Sync>,
}

#[derive(Clone)]
pub(crate) struct FrameRouter {
    shared: Arc<CaptureShared>,
    workers: Arc<CaptureWorkers>,
}

struct CaptureShared {
    routes: DashMap<u64, Watchable<Option<FrameReady>>>,
    revoked: DashSet<u64>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
    listener: LocalSocketListener,
    sockets: DashMap<u64, LocalSocket>,
    stopping: AtomicBool,
}

struct CaptureWorkers {
    shared: Arc<CaptureShared>,
    acceptor: Mutex<Option<JoinHandle<()>>>,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl FrameRouter {
    pub(crate) fn wake(&self) {
        if let Some(wake) = &self.shared.wake {
            wake();
        }
    }

    pub(crate) fn route(&self, connection: u64, frames: Watchable<Option<FrameReady>>) {
        self.shared.revoked.remove(&connection);
        self.shared.routes.insert(connection, frames);
    }

    pub(crate) fn revoke(&self, connection: u64) {
        self.shared.revoked.insert(connection);
        self.shared.routes.remove(&connection);
        if let Some((_, socket)) = self.shared.sockets.remove(&connection) {
            let _ = socket.shutdown();
        }
    }

    pub(crate) fn shutdown(&self) {
        self.workers.shutdown();
    }
}

impl CaptureShared {
    fn revoked(&self, connection: u64) -> bool {
        self.revoked.contains(&connection)
    }

    fn publish(&self, connection: u64, frame: FrameReady) {
        let sender = self
            .routes
            .get(&connection)
            .map(|sender| sender.value().clone());
        if let Some(sender) = sender {
            sender.replace(Some(frame));
            if let Some(wake) = &self.wake {
                wake();
            }
        }
    }

    fn revoke(&self, connection: u64) {
        self.revoked.insert(connection);
        self.routes.remove(&connection);
        self.sockets.remove(&connection);
    }
}

impl CaptureWorkers {
    fn shutdown(&self) {
        if self.shared.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        self.shared.listener.close();
        for socket in &self.shared.sockets {
            let _ = socket.value().shutdown();
        }
        if let Some(acceptor) = self.acceptor.lock().take() {
            let _ = acceptor.join();
        }
        for worker in self.connections.lock().drain(..) {
            let _ = worker.join();
        }
        self.shared.routes.clear();
        self.shared.sockets.clear();
    }
}

impl Drop for CaptureWorkers {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(crate) fn bind(
    session: &str,
    preference: ConsumerPreference,
    callbacks: CaptureCallbacks,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
) -> std::io::Result<FrameRouter> {
    let listener = LocalSocketListener::bind(session)?;
    let shared = Arc::new(CaptureShared {
        routes: DashMap::new(),
        revoked: DashSet::new(),
        wake,
        listener: listener.clone(),
        sockets: DashMap::new(),
        stopping: AtomicBool::new(false),
    });
    let connections = Arc::new(Mutex::new(Vec::new()));
    let acceptor = spawn_acceptor(
        listener,
        callbacks,
        preference,
        shared.clone(),
        connections.clone(),
    )?;
    let workers = Arc::new(CaptureWorkers {
        shared: shared.clone(),
        acceptor: Mutex::new(Some(acceptor)),
        connections,
    });
    Ok(FrameRouter { shared, workers })
}

fn spawn_acceptor(
    listener: LocalSocketListener,
    callbacks: CaptureCallbacks,
    preference: ConsumerPreference,
    shared: Arc<CaptureShared>,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("snowglass-runtime-capture-accept".to_owned())
        .spawn(move || {
            let next_connection = AtomicU64::new(0);
            loop {
                let mut socket = match listener.accept() {
                    Ok(socket) => socket,
                    Err(error) => {
                        if !shared.stopping.load(Ordering::Acquire) {
                            eprintln!("snowglass: capture IPC accept ended: {error}");
                        }
                        return;
                    }
                };
                if shared.stopping.load(Ordering::Acquire) {
                    let _ = socket.shutdown();
                    return;
                }
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
                if let Ok(shutdown_socket) = socket.try_clone() {
                    shared.sockets.insert(connection, shutdown_socket);
                }
                let worker_callbacks = callbacks.clone();
                let worker_shared = shared.clone();
                let worker = thread::Builder::new()
                    .name(format!("snowglass-runtime-capture-{connection}"))
                    .spawn(move || {
                        receive_connection(connection, socket, worker_callbacks, &worker_shared);
                    });
                match worker {
                    Ok(worker) => connections.lock().push(worker),
                    Err(error) => {
                        shared.revoke(connection);
                        eprintln!(
                            "snowglass: failed to start capture connection {connection}: {error}"
                        );
                    }
                }
            }
        })
}

fn receive_connection(
    connection: u64,
    mut socket: LocalSocket,
    callbacks: CaptureCallbacks,
    shared: &CaptureShared,
) {
    let result = (|| -> std::io::Result<()> {
        let mut accepted_stream = false;
        let mut frames = None;
        loop {
            let message = socket.receive()?;
            if shared.revoked(connection) || shared.stopping.load(Ordering::Acquire) {
                return Ok(());
            }
            match message {
                Message::StreamReady(stream) => {
                    let (accepted, mut response) = oneshot::oneshot();
                    (callbacks.offer)(connection, stream, accepted);
                    accepted_stream = loop {
                        match response.try_recv() {
                            Ok(accepted) => break accepted,
                            Err(crossfire::TryRecvError::Empty)
                                if !shared.stopping.load(Ordering::Acquire) =>
                            {
                                thread::sleep(std::time::Duration::from_millis(1));
                            }
                            Err(_) => break false,
                        }
                    };
                    if accepted_stream {
                        socket.send(Message::StreamAccepted)?;
                    } else {
                        return Ok(());
                    }
                }
                Message::FrameReady(frame) if accepted_stream => {
                    shared.publish(connection, frame);
                }
                Message::FrameAvailable if accepted_stream => {
                    if let Some(frame) = frames.as_mut().and_then(
                        |frames: &mut snowglass_ipc::frame::FrameSubscriber| {
                            frames.latest().ok().flatten()
                        },
                    ) {
                        shared.publish(connection, frame);
                    }
                }
                Message::MapOffer(offer)
                    if offer.channel == snowglass_ipc::control::ChannelKind::Frame =>
                {
                    frames = Some(snowglass_ipc::frame::FrameSubscriber::from_offer(offer)?);
                }
                Message::Shutdown => return Ok(()),
                _ => {}
            }
        }
    })();
    if let Err(error) = result
        && !shared.stopping.load(Ordering::Acquire)
        && error.kind() != std::io::ErrorKind::UnexpectedEof
        && error.kind() != std::io::ErrorKind::ConnectionReset
    {
        eprintln!("snowglass: capture IPC connection {connection} ended: {error}");
    }
    shared.revoke(connection);
    (callbacks.disconnected)(connection);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_joins_workers_and_releases_listener() {
        let session = format!("snowglass-runtime-capture-test-{}", std::process::id());
        let callbacks = CaptureCallbacks {
            offer: Arc::new(|_, _, accepted| accepted.send(false)),
            disconnected: Arc::new(|_| {}),
        };
        let router = bind(&session, ConsumerPreference::D3d12, callbacks, None).unwrap();
        router.shutdown();
        let replacement = LocalSocketListener::bind(&session).unwrap();
        replacement.close();
    }
}
