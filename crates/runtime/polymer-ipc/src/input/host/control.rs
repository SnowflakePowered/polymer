use super::{InputIpcListener, topology};
use crate::{BlobKind, LocalSocket, Message};
use parking_lot::Mutex;
use polymer_controllers::topology::InputTopology;
use polymer_input::{Feedback, ListenerHandle, PlayerIndex};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

#[derive(Default)]
pub struct Control {
    clients: Arc<Mutex<Vec<LocalSocket>>>,
    topology: Arc<Mutex<Vec<u8>>>,
    stopping: Arc<AtomicBool>,
}

impl Control {
    pub fn spawn_acceptor(&self, listener: InputIpcListener) -> io::Result<JoinHandle<()>> {
        thread::Builder::new()
            .name("polymer-input-ipc-accept".to_owned())
            .spawn(move || {
                while !listener.0.stopping.load(Ordering::Acquire) {
                    let Ok(mut socket) = listener.0.listener.accept() else {
                        return;
                    };
                    if let Err(error) = listener.accept(&mut socket) {
                        tracing::warn!("input client rejected: {error}");
                    }
                }
            })
    }

    pub fn add_client(
        &self,
        socket: &LocalSocket,
        feedback: Arc<Mutex<Option<ListenerHandle>>>,
    ) -> io::Result<()> {
        let mut writer = socket.try_clone()?;
        let topology = self.topology.lock().clone();
        if !topology.is_empty() {
            writer.send_blob(BlobKind::Topology, &topology)?;
        }
        let mut reader = socket.try_clone()?;
        thread::Builder::new()
            .name("polymer-input-ipc-feedback".to_owned())
            .spawn(move || {
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
            })?;
        self.clients.lock().push(writer);
        Ok(())
    }

    pub fn publish_topology(&self, value: &InputTopology) -> io::Result<()> {
        let bytes = topology::encode(value)?;
        *self.topology.lock() = bytes.clone();
        let mut clients = self.clients.lock();
        clients.retain_mut(|client| client.send_blob(BlobKind::Topology, &bytes).is_ok());
        Ok(())
    }

    pub fn send(&self, message: Message) -> bool {
        let mut clients = self.clients.lock();
        clients.retain_mut(|client| client.send(message).is_ok());
        !clients.is_empty()
    }

    pub fn shutdown(&self) {
        if self.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        for client in self.clients.lock().drain(..) {
            let _ = client.shutdown();
        }
    }
}
