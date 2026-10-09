use super::{InputIpcListener, topology};
use crate::{BlobKind, LocalSocket, Message};
use dashmap::DashMap;
use parking_lot::Mutex;
use polymer_controllers::topology::InputTopology;
use polymer_input::{Feedback, ListenerHandle, PlayerIndex};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct Control {
    clients: Arc<Mutex<Vec<LocalSocket>>>,
    handshaking: DashMap<u64, LocalSocket>,
    next_handshake: AtomicU64,
    topology: Arc<Mutex<Vec<u8>>>,
    stopping: Arc<AtomicBool>,
}

impl Control {
    pub fn spawn_acceptor(&self, listener: InputIpcListener) -> io::Result<JoinHandle<()>> {
        thread::Builder::new()
            .name("polymer-input-ipc-accept".to_owned())
            .spawn(move || {
                while !listener.0.stopping.load(Ordering::Acquire) {
                    let Ok(socket) = listener.0.listener.accept() else {
                        return;
                    };
                    let client = listener.clone();
                    if let Err(error) = thread::Builder::new()
                        .name("polymer-input-ipc-client".to_owned())
                        .spawn(move || client.serve(socket))
                    {
                        tracing::warn!("input client thread failed to start: {error}");
                    }
                }
            })
    }

    pub fn handshake(
        &self,
        shutdown: LocalSocket,
        run: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        let id = self.next_handshake.fetch_add(1, Ordering::Relaxed);
        self.handshaking.insert(id, shutdown);
        let result = if self.stopping.load(Ordering::Acquire) {
            Err(io::Error::from(io::ErrorKind::Interrupted))
        } else {
            run()
        };
        self.handshaking.remove(&id);
        result
    }

    pub fn add_client(&self, socket: &LocalSocket) -> io::Result<()> {
        let mut writer = socket.try_clone()?;
        writer.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let topology = self.topology.lock().clone();
        if !topology.is_empty() {
            writer.send(Message::Blob {
                kind: BlobKind::Topology,
                data: topology,
            })?;
        }
        let shutdown = writer.try_clone()?;
        self.clients.lock().push(writer);
        if self.stopping.load(Ordering::Acquire) {
            let _ = shutdown.shutdown();
        }
        Ok(())
    }

    pub fn forward_feedback(mut reader: LocalSocket, feedback: &Mutex<Option<ListenerHandle>>) {
        while let Ok(message) = reader.receive() {
            let Message::GamepadFeedback(value) = message else {
                continue;
            };
            if let Some(handle) = feedback.lock().as_ref() {
                let _ = handle.feedback(
                    PlayerIndex::new(value.player),
                    Feedback::Rumble {
                        low_frequency: value.low_frequency,
                        high_frequency: value.high_frequency,
                        duration_ms: value.duration_ms,
                    },
                );
            }
        }
    }

    pub fn publish_topology(&self, value: &InputTopology) -> io::Result<()> {
        let bytes = topology::encode(value)?;
        *self.topology.lock() = bytes.clone();
        self.send(Message::Blob {
            kind: BlobKind::Topology,
            data: bytes,
        });
        Ok(())
    }

    pub fn send(&self, message: Message) -> bool {
        let mut clients = self.clients.lock();
        clients.retain_mut(|client| {
            let sent = client.send(message.clone()).is_ok();
            if !sent {
                let _ = client.shutdown();
            }
            sent
        });
        !clients.is_empty()
    }

    pub fn shutdown(&self) {
        if self.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        for client in self.clients.lock().drain(..) {
            let _ = client.shutdown();
        }
        for client in self.handshaking.iter() {
            let _ = client.shutdown();
        }
    }
}
