//! Game input transport owned by the shell.
//!
//! Policy remains in the shell. `snowglass-input-dinput` is used only to
//! encode mouse/keyboard state changes for the injected IPC implementation.

use snowglass_input::{
    gamepad::LatestGamepadMailbox,
    provider::sdl3::{ProviderConfig, Sdl3Provider},
};
use snowglass_ipc::{LocalSocket, LocalSocketListener, Message, input::InputPublisher};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
    time::Duration,
};

const CONTROL_CLIENT_QUEUE: usize = 4096;

pub struct InputTransport {
    mailbox: Arc<LatestGamepadMailbox>,
    control: mpsc::Sender<Message>,
    dispatch_latency: Arc<Mutex<VecDeque<Duration>>>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DispatchSummary {
    pub samples: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

impl InputTransport {
    pub fn bind(session: &str) -> std::io::Result<Self> {
        let listener = LocalSocketListener::bind(&format!("{session}-input"))?;
        let mailbox = Arc::new(LatestGamepadMailbox::new(u8::MAX as usize + 1));
        let publisher = Arc::new(Mutex::new(InputPublisher::new()?));
        let clients = Arc::new(Mutex::new(Vec::<SyncSender<Message>>::new()));
        spawn_acceptor(listener, publisher.clone(), clients.clone());
        let dispatch_latency = Arc::new(Mutex::new(VecDeque::new()));
        spawn_gamepad_publisher(mailbox.clone(), publisher, dispatch_latency.clone());
        let (control, receiver) = mpsc::channel();
        spawn_control_publisher(receiver, clients);
        Ok(Self {
            mailbox,
            control,
            dispatch_latency,
        })
    }

    pub fn start_provider(&self, config: ProviderConfig) -> Result<Sdl3Provider, String> {
        Sdl3Provider::start(config, self.mailbox.clone())
    }

    pub fn control_sender(&self) -> mpsc::Sender<Message> {
        self.control.clone()
    }

    pub fn dispatch_summary(&self) -> DispatchSummary {
        let samples = self
            .dispatch_latency
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut values = samples
            .iter()
            .map(Duration::as_secs_f64)
            .collect::<Vec<_>>();
        values.sort_by(f64::total_cmp);
        let percentile = |fraction: f64| {
            values
                .get(((values.len().saturating_sub(1)) as f64 * fraction).round() as usize)
                .copied()
                .unwrap_or_default()
                * 1000.0
        };
        DispatchSummary {
            samples: values.len(),
            p50_ms: percentile(0.50),
            p95_ms: percentile(0.95),
            p99_ms: percentile(0.99),
        }
    }
}

fn spawn_acceptor(
    listener: LocalSocketListener,
    publisher: Arc<Mutex<InputPublisher>>,
    clients: Arc<Mutex<Vec<SyncSender<Message>>>>,
) {
    thread::Builder::new()
        .name("snowglass-shell-input-accept".to_owned())
        .spawn(move || {
            loop {
                let mut socket = match listener.accept() {
                    Ok(socket) => socket,
                    Err(error) => {
                        eprintln!("snowglass: input IPC accept ended: {error}");
                        return;
                    }
                };
                let _ = socket.set_read_timeout(Some(Duration::from_secs(1)));
                match publisher
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .accept(&mut socket)
                {
                    Ok(process_id) => {
                        eprintln!("snowglass: target input IPC connected for process {process_id}");
                        let _ = socket.set_read_timeout(None);
                        clients
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .push(spawn_control_client(socket, process_id));
                    }
                    Err(error) => eprintln!("snowglass: input IPC handshake failed: {error}"),
                }
            }
        })
        .expect("failed to spawn shell input acceptor");
}

fn spawn_control_client(mut socket: LocalSocket, process_id: u32) -> SyncSender<Message> {
    let (sender, receiver) = mpsc::sync_channel(CONTROL_CLIENT_QUEUE);
    thread::Builder::new()
        .name(format!("snowglass-shell-control-client-{process_id}"))
        .spawn(move || {
            while let Ok(message) = receiver.recv() {
                if socket.send(message).is_err() {
                    break;
                }
            }
        })
        .expect("failed to spawn shell input control client");
    sender
}

fn spawn_gamepad_publisher(
    mailbox: Arc<LatestGamepadMailbox>,
    publisher: Arc<Mutex<InputPublisher>>,
    dispatch_latency: Arc<Mutex<VecDeque<Duration>>>,
) {
    thread::Builder::new()
        .name("snowglass-shell-gamepad-publish".to_owned())
        .spawn(move || {
            let mut sequences = vec![0u32; u8::MAX as usize + 1];
            loop {
                for stamped in mailbox.wait_and_drain() {
                    let mut update = stamped.update;
                    let sequence = &mut sequences[update.slot.index()];
                    *sequence = sequence.wrapping_add(1);
                    update.sequence = *sequence;
                    if let Err(error) = publisher
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .publish(update)
                    {
                        eprintln!("snowglass: gamepad input publish failed: {error}");
                    } else {
                        let mut samples = dispatch_latency
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        samples.push_back(stamped.sampled_at.elapsed());
                        if samples.len() > 1024 {
                            samples.pop_front();
                        }
                    }
                }
            }
        })
        .expect("failed to spawn shell gamepad publisher");
}

fn spawn_control_publisher(
    receiver: mpsc::Receiver<Message>,
    clients: Arc<Mutex<Vec<SyncSender<Message>>>>,
) {
    thread::Builder::new()
        .name("snowglass-shell-control-publish".to_owned())
        .spawn(move || {
            while let Ok(message) = receiver.recv() {
                broadcast_control(
                    &mut clients.lock().unwrap_or_else(|error| error.into_inner()),
                    message,
                );
            }
        })
        .expect("failed to spawn shell control publisher");
}

fn broadcast_control(clients: &mut Vec<SyncSender<Message>>, message: Message) {
    clients.retain(|client| match client.try_send(message) {
        Ok(()) => true,
        Err(TrySendError::Disconnected(_)) => false,
        Err(TrySendError::Full(_)) => {
            eprintln!("snowglass: dropping an unresponsive input control client");
            false
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_control_client_does_not_block_other_clients() {
        let (blocked_sender, _blocked_receiver) = mpsc::sync_channel(1);
        let (active_sender, active_receiver) = mpsc::sync_channel(2);
        let mut clients = vec![blocked_sender, active_sender];

        broadcast_control(&mut clients, Message::Shutdown);
        assert_eq!(active_receiver.recv().unwrap(), Message::Shutdown);

        broadcast_control(&mut clients, Message::FrameAvailable);
        assert_eq!(active_receiver.recv().unwrap(), Message::FrameAvailable);
        assert_eq!(clients.len(), 1);
    }
}
