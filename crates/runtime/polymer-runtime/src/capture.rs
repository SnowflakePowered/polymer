//! Target-to-viewer capture transport built on the existing Polymer IPC.

use crossfire::oneshot;
use dashmap::{DashMap, DashSet};
use parking_lot::Mutex;
use polymer_ipc::{LocalSocket, LocalSocketListener, Message};
use polymer_protocol::{ConsumerApi, FrameReady, Handshake, PROTOCOL_VERSION};
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
    Metal,
}

#[derive(Clone)]
pub(crate) struct CaptureCallbacks {
    pub(crate) offer:
        Arc<dyn Fn(u64, polymer_ipc::StreamReady, oneshot::TxOneshot<bool>) + Send + Sync>,
    pub(crate) disconnected: Arc<dyn Fn(u64) + Send + Sync>,
}

#[derive(Clone)]
pub(crate) struct FrameRouter {
    shared: Arc<CaptureShared>,
    workers: Arc<CaptureWorkers>,
}

struct CaptureShared {
    #[cfg(target_os = "macos")]
    resources: DashMap<u64, polymer_ipc::xpc::MetalResources>,
    availability: DashMap<u64, Arc<Mutex<LocalSocket>>>,
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
    #[cfg(target_os = "macos")]
    pub(crate) fn metal_resources(
        &self,
        connection: u64,
    ) -> Option<polymer_ipc::xpc::MetalResources> {
        self.shared.resources.get(&connection).map(|v| v.clone())
    }

    pub(crate) fn buffer_release_sender(&self, connection: u64) -> BufferReleaseSender {
        BufferReleaseSender {
            shared: Arc::downgrade(&self.shared),
            connection,
        }
    }
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
        self.shared.availability.remove(&connection);
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
        #[cfg(target_os = "macos")]
        self.resources.remove(&connection);
        self.availability.remove(&connection);
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
        self.shared.availability.clear();
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
        #[cfg(target_os = "macos")]
        resources: DashMap::new(),
        availability: DashMap::new(),
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
        .name("polymer-runtime-capture-accept".to_owned())
        .spawn(move || {
            let next_connection = AtomicU64::new(0);
            loop {
                let mut socket = match listener.accept() {
                    Ok(socket) => socket,
                    Err(error) => {
                        if !shared.stopping.load(Ordering::Acquire) {
                            tracing::error!("polymer: capture IPC accept ended: {error}");
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
                    ConsumerPreference::Metal => ConsumerApi::Metal,
                };
                if let Err(error) = socket.send(Message::Hello(Handshake {
                    protocol_version: PROTOCOL_VERSION,
                    process_id: std::process::id(),
                    preferred_consumer,
                })) {
                    tracing::error!("polymer: capture IPC handshake failed: {error}");
                    continue;
                }
                if let Ok(shutdown_socket) = socket.try_clone() {
                    shared.sockets.insert(connection, shutdown_socket);
                }
                let worker_callbacks = callbacks.clone();
                let worker_shared = shared.clone();
                let worker = thread::Builder::new()
                    .name(format!("polymer-runtime-capture-{connection}"))
                    .spawn(move || {
                        receive_connection(connection, socket, worker_callbacks, &worker_shared);
                    });
                match worker {
                    Ok(worker) => connections.lock().push(worker),
                    Err(error) => {
                        shared.revoke(connection);
                        tracing::error!(
                            "polymer: failed to start capture connection {connection}: {error}"
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
        loop {
            let message = socket.receive()?;
            if shared.revoked(connection) || shared.stopping.load(Ordering::Acquire) {
                return Ok(());
            }
            match message {
                Message::RequestBufferAvailability if !accepted_stream => {
                    let mut sender = socket.try_clone()?;
                    sender.set_write_timeout(Some(std::time::Duration::from_secs(2)))?;
                    shared
                        .availability
                        .insert(connection, Arc::new(Mutex::new(sender)));
                }
                Message::StreamReady(stream) => {
                    #[cfg(target_os = "macos")]
                    shared.resources.insert(
                        connection,
                        socket
                            .take_stream_resources()
                            .ok_or_else(|| std::io::Error::other("missing Metal resources"))?,
                    );
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
        tracing::error!("polymer: capture IPC connection {connection} ended: {error}");
    }
    shared.revoke(connection);
    (callbacks.disconnected)(connection);
}

/// A release endpoint bound to a single capture connection, never to a mutable
/// surface identity. Old imported resources cannot acknowledge a replacement.
#[derive(Clone)]
pub struct BufferReleaseSender {
    shared: std::sync::Weak<CaptureShared>,
    connection: u64,
}

impl BufferReleaseSender {
    pub fn release(
        &self,
        stream_id: polymer_protocol::StreamId,
        through_sync_value: u64,
    ) -> std::io::Result<()> {
        let Some(shared) = self.shared.upgrade() else {
            return Ok(());
        };
        let sender = shared
            .availability
            .get(&self.connection)
            .map(|entry| entry.value().clone());
        if let Some(sender) = sender {
            sender.lock().send(Message::BufferAvailable {
                stream_id,
                through_sync_value,
            })?;
        }
        Ok(())
    }
}
