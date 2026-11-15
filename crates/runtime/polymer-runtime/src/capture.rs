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

#[cfg(windows)]
use polymer_consumer_d3d12::ProducerTimeline;
#[cfg(target_os = "macos")]
use polymer_consumer_metal::ProducerTimeline;

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
    #[cfg(windows)]
    resources: DashMap<u64, Arc<polymer_ipc::Win32Resources>>,
    routes: DashMap<u64, Watchable<Option<FrameReady>>>,
    timelines: DashMap<u64, Arc<ProducerTimeline>>,
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

    #[cfg(windows)]
    pub(crate) fn win32_resources(
        &self,
        connection: u64,
    ) -> Option<Arc<polymer_ipc::Win32Resources>> {
        self.shared.resources.get(&connection).map(|v| v.clone())
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
        self.shared.revoked.insert(connection);
        self.shared.routes.remove(&connection);
        self.shared.close_timeline(connection);
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
        self.resources.remove(&connection);
        self.revoked.insert(connection);
        self.routes.remove(&connection);
        self.sockets.remove(&connection);
        self.close_timeline(connection);
    }

    fn close_timeline(&self, connection: u64) {
        if let Some((_, timeline)) = self.timelines.remove(&connection) {
            timeline.close();
        }
    }

    /// Publishes each newly completed value of the producer's ready timeline.
    /// The timeline alone identifies the newest renderable slot.
    fn watch_frames(&self, connection: u64, timeline: &ProducerTimeline) {
        let mut observed = 0;
        while let Some(value) = timeline.wait_beyond(observed) {
            observed = value;
            self.publish(connection, FrameReady::observed(value));
        }
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
        for timeline in &self.shared.timelines {
            timeline.value().close();
        }
        if let Some(acceptor) = self.acceptor.lock().take() {
            let _ = acceptor.join();
        }
        let workers = std::mem::take(&mut *self.connections.lock());
        for worker in workers {
            let _ = worker.join();
        }
        self.shared.routes.clear();
        self.shared.sockets.clear();
        self.shared.timelines.clear();
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
        resources: DashMap::new(),
        routes: DashMap::new(),
        timelines: DashMap::new(),
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
                let worker_connections = connections.clone();
                let worker = thread::Builder::new()
                    .name(format!("polymer-runtime-capture-{connection}"))
                    .spawn(move || {
                        receive_connection(
                            connection,
                            socket,
                            worker_callbacks,
                            &worker_shared,
                            &worker_connections,
                        );
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
    shared: &Arc<CaptureShared>,
    connections: &Mutex<Vec<JoinHandle<()>>>,
) {
    let result = (|| -> std::io::Result<()> {
        loop {
            let message = socket.receive()?;
            if shared.revoked(connection) || shared.stopping.load(Ordering::Acquire) {
                return Ok(());
            }
            let Message::StreamReady(stream) = message else {
                continue;
            };
            #[cfg(target_os = "macos")]
            let timeline = {
                let resources = socket
                    .take_stream_resources()
                    .ok_or_else(|| std::io::Error::other("missing Metal resources"))?;
                let timeline = ProducerTimeline::open(&resources)?;
                shared.resources.insert(connection, resources);
                timeline
            };
            #[cfg(windows)]
            let timeline = {
                let resources = socket
                    .take_stream_resources()
                    .ok_or_else(|| std::io::Error::other("missing Win32 resources"))?;
                let timeline =
                    ProducerTimeline::open(&stream, &resources).map_err(std::io::Error::other)?;
                shared.resources.insert(connection, Arc::new(resources));
                timeline
            };
            let (accepted, response) = oneshot::oneshot();
            (callbacks.offer)(connection, stream, accepted);
            // Every offer is resolved, and shutdown rejects pending ones
            // before joining this thread.
            let accepted_stream = response.recv().unwrap_or(false);
            if !accepted_stream {
                return Ok(());
            }
            let timeline = Arc::new(timeline);
            shared.timelines.insert(connection, timeline.clone());
            let watcher_shared = shared.clone();
            let mut workers = connections.lock();
            if shared.stopping.load(Ordering::Acquire) {
                return Ok(());
            }
            workers.push(
                thread::Builder::new()
                    .name(format!("polymer-runtime-frames-{connection}"))
                    .spawn(move || watcher_shared.watch_frames(connection, &timeline))?,
            );
            drop(workers);
            socket.send(Message::StreamAccepted)?;
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
