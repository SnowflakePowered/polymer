//! Viewer-to-target input transport using the existing Snowglass IPC paths.

use crossfire::{MTx, Rx, mpsc};
use parking_lot::Mutex;
use snowglass_input::gamepad::{
    GamepadFeedback, GamepadUpdate, GamepadUpdateSink, LatestGamepadMailbox,
    VirtualGamepadConnection,
};
use snowglass_ipc::{LocalSocket, LocalSocketListener, Message, input::InputPublisher};
use std::sync::mpsc as std_mpsc;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

type ControlTx = MTx<mpsc::List<ControlCommand>>;
type ControlRx = Rx<mpsc::List<ControlCommand>>;

const CONTROL_WRITE_TIMEOUT: Duration = Duration::from_millis(100);

enum ControlCommand {
    Send(Message),
    Stop,
}

#[doc(hidden)]
#[derive(Clone)]
pub struct ProtocolInputSender(ControlTx);

impl ProtocolInputSender {
    pub fn send(&self, message: Message) -> bool {
        self.0.send(ControlCommand::Send(message)).is_ok()
    }
}

#[derive(Clone)]
pub(crate) struct InputTransport(Arc<InputInner>);

struct InputInner {
    control: ControlTx,
    mailbox: Arc<LatestGamepadMailbox>,
    gamepads: Arc<Mutex<InputPublisher>>,
    listener: LocalSocketListener,
    stopping: Arc<AtomicBool>,
    clients: Arc<Mutex<Vec<LocalSocket>>>,
    pending_socket: Arc<Mutex<Option<LocalSocket>>>,
    feedback_subscribers: Arc<Mutex<Vec<std_mpsc::Sender<GamepadFeedback>>>>,
    workers: Mutex<Option<InputWorkers>>,
}

struct InputWorkers {
    acceptor: JoinHandle<()>,
    gamepad: JoinHandle<()>,
    control: JoinHandle<()>,
}

impl InputTransport {
    pub(crate) fn bind(session: &str) -> std::io::Result<Self> {
        let listener = LocalSocketListener::bind(&format!("{session}-input"))?;
        let gamepads = Arc::new(Mutex::new(InputPublisher::new()?));
        let mailbox = Arc::new(LatestGamepadMailbox::new(u8::MAX as usize + 1));
        let stopping = Arc::new(AtomicBool::new(false));
        let clients = Arc::new(Mutex::new(Vec::<LocalSocket>::new()));
        let pending_socket = Arc::new(Mutex::new(None));
        let feedback_subscribers = Arc::new(Mutex::new(Vec::new()));
        let (control, receiver) = mpsc::unbounded_blocking();

        let acceptor = spawn_acceptor(
            listener.clone(),
            gamepads.clone(),
            clients.clone(),
            pending_socket.clone(),
            feedback_subscribers.clone(),
            stopping.clone(),
        );
        let gamepad = spawn_gamepad_writer(mailbox.clone(), gamepads.clone(), stopping.clone());
        let control_worker = spawn_control_writer(receiver, clients.clone(), stopping.clone());
        let workers = match (acceptor, gamepad, control_worker) {
            (Ok(acceptor), Ok(gamepad), Ok(control)) => InputWorkers {
                acceptor,
                gamepad,
                control,
            },
            (acceptor, gamepad, control_worker) => {
                stopping.store(true, Ordering::Release);
                listener.close();
                wake_gamepad_writer(&mailbox);
                let _ = control.send(ControlCommand::Stop);
                if let Some(socket) = pending_socket.lock().as_ref() {
                    let _ = socket.shutdown();
                }
                for socket in clients.lock().iter() {
                    let _ = socket.shutdown();
                }
                for worker in [acceptor.ok(), gamepad.ok(), control_worker.ok()]
                    .into_iter()
                    .flatten()
                {
                    let _ = worker.join();
                }
                return Err(std::io::Error::other(
                    "failed to start one or more input workers",
                ));
            }
        };
        Ok(Self(Arc::new(InputInner {
            control,
            mailbox,
            gamepads,
            listener,
            stopping,
            clients,
            pending_socket,
            feedback_subscribers,
            workers: Mutex::new(Some(workers)),
        })))
    }

    pub(crate) fn send(&self, message: Message) -> bool {
        self.0.control.send(ControlCommand::Send(message)).is_ok()
    }

    pub(crate) fn sender(&self) -> ProtocolInputSender {
        ProtocolInputSender(self.0.control.clone())
    }

    pub(crate) fn subscribe_feedback(&self) -> std_mpsc::Receiver<GamepadFeedback> {
        let (sender, receiver) = std_mpsc::channel();
        self.0.feedback_subscribers.lock().push(sender);
        receiver
    }

    pub(crate) fn publish_gamepad(&self, update: GamepadUpdate) -> std::io::Result<()> {
        self.0.mailbox.publish(update);
        Ok(())
    }

    pub(crate) fn set_gamepad_connected(
        &self,
        connection: VirtualGamepadConnection,
    ) -> std::io::Result<()> {
        self.0.gamepads.lock().set_connected(connection)
    }

    pub(crate) fn shutdown(&self) {
        self.0.shutdown();
    }
}

impl GamepadUpdateSink for InputTransport {
    fn publish(&self, update: GamepadUpdate) {
        let _ = self.publish_gamepad(update);
    }
}

impl InputInner {
    fn shutdown(&self) {
        if self.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        self.listener.close();
        wake_gamepad_writer(&self.mailbox);
        let _ = self.control.send(ControlCommand::Send(Message::Shutdown));
        let _ = self.control.send(ControlCommand::Stop);

        let Some(workers) = self.workers.lock().take() else {
            return;
        };
        if let Some(socket) = self.pending_socket.lock().as_ref() {
            let _ = socket.shutdown();
        }
        for socket in self.clients.lock().iter() {
            let _ = socket.shutdown();
        }
        let _ = workers.control.join();
        let _ = workers.acceptor.join();
        let _ = workers.gamepad.join();
    }
}

impl Drop for InputInner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn wake_gamepad_writer(mailbox: &LatestGamepadMailbox) {
    mailbox.publish(GamepadUpdate {
        slot: snowglass_input::gamepad::GamepadSlot::new(0).expect("slot zero is valid"),
        sequence: u32::MAX,
        connected: false,
        state: snowglass_input::gamepad::GamepadState::default(),
    });
}

fn spawn_gamepad_writer(
    mailbox: Arc<LatestGamepadMailbox>,
    gamepads: Arc<Mutex<InputPublisher>>,
    stopping: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    let metrics = snowglass_statistics::InputMetrics::new("sdl3");
    thread::Builder::new()
        .name("snowglass-runtime-gamepad-send".to_owned())
        .spawn(move || {
            loop {
                for update in mailbox.wait_and_drain() {
                    if stopping.load(Ordering::Acquire) {
                        return;
                    }
                    let _ = gamepads.lock().publish(update.update);
                    metrics.record_dispatch(update.sampled_at.elapsed());
                }
            }
        })
}

fn spawn_acceptor(
    listener: LocalSocketListener,
    gamepads: Arc<Mutex<InputPublisher>>,
    clients: Arc<Mutex<Vec<LocalSocket>>>,
    pending_socket: Arc<Mutex<Option<LocalSocket>>>,
    feedback_subscribers: Arc<Mutex<Vec<std_mpsc::Sender<GamepadFeedback>>>>,
    stopping: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("snowglass-runtime-input-accept".to_owned())
        .spawn(move || {
            loop {
                let mut socket = match listener.accept() {
                    Ok(socket) => socket,
                    Err(error) => {
                        if !stopping.load(Ordering::Acquire) {
                            tracing::error!("snowglass: input IPC accept ended: {error}");
                        }
                        return;
                    }
                };
                if stopping.load(Ordering::Acquire) {
                    let _ = socket.shutdown();
                    return;
                }
                *pending_socket.lock() = socket.try_clone().ok();
                let _ = socket.set_read_timeout(Some(Duration::from_secs(1)));
                let accepted = gamepads.lock().accept(&mut socket).is_ok();
                pending_socket.lock().take();
                if accepted && !stopping.load(Ordering::Acquire) {
                    let _ = socket.set_read_timeout(None);
                    if let Err(error) =
                        socket.set_write_timeout(Some(CONTROL_WRITE_TIMEOUT))
                    {
                        tracing::error!(
                            "snowglass: input IPC client rejected because writes could not be bounded: {error}"
                        );
                        let _ = socket.shutdown();
                        continue;
                    }
                    if let Ok(feedback_socket) = socket.try_clone() {
                        spawn_feedback_reader(feedback_socket, feedback_subscribers.clone());
                    }
                    clients.lock().push(socket);
                } else if stopping.load(Ordering::Acquire) {
                    return;
                }
            }
        })
}

fn spawn_feedback_reader(
    mut socket: LocalSocket,
    subscribers: Arc<Mutex<Vec<std_mpsc::Sender<GamepadFeedback>>>>,
) {
    let _ = thread::Builder::new()
        .name("snowglass-runtime-gamepad-feedback".to_owned())
        .spawn(move || {
            while let Ok(message) = socket.receive() {
                let Message::GamepadFeedback(feedback) = message else {
                    continue;
                };
                subscribers
                    .lock()
                    .retain(|subscriber| subscriber.send(feedback).is_ok());
            }
        });
}

fn spawn_control_writer(
    receiver: ControlRx,
    clients: Arc<Mutex<Vec<LocalSocket>>>,
    stopping: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("snowglass-runtime-input-send".to_owned())
        .spawn(move || {
            while let Ok(command) = receiver.recv() {
                let ControlCommand::Send(message) = command else {
                    return;
                };
                // Never leave a stale injected client at the head of the
                // process-wide input stream. Take ownership while writing so
                // the acceptor and shutdown paths do not wait on this lock,
                // and retain only clients that accepted the message within
                // the bounded socket timeout.
                let mut active = std::mem::take(&mut *clients.lock());
                active.retain_mut(|socket| match socket.send(message) {
                    Ok(()) => true,
                    Err(error) => {
                        if !stopping.load(Ordering::Acquire)
                            && !matches!(
                                error.kind(),
                                std::io::ErrorKind::BrokenPipe
                                    | std::io::ErrorKind::ConnectionReset
                                    | std::io::ErrorKind::TimedOut
                                    | std::io::ErrorKind::WouldBlock
                            )
                        {
                            tracing::error!("snowglass: input IPC send failed: {error}");
                        }
                        let _ = socket.shutdown();
                        false
                    }
                });
                clients.lock().extend(active);
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use snowglass_input::gamepad::GamepadSlot;
    use snowglass_ipc::input::InputSubscriber;
    use std::time::Instant;

    #[test]
    fn shutdown_joins_workers_and_releases_listener() {
        let session = format!("snowglass-runtime-input-test-{}", std::process::id());
        let transport = InputTransport::bind(&session).unwrap();
        transport.shutdown();
        let replacement = LocalSocketListener::bind(&format!("{session}-input")).unwrap();
        replacement.close();
    }

    #[test]
    fn receives_virtual_gamepad_feedback_from_a_client() {
        let session = format!("snowglass-runtime-feedback-test-{}", std::process::id());
        let transport = InputTransport::bind(&session).unwrap();
        let feedback = transport.subscribe_feedback();
        let mut client = InputSubscriber::connect(&format!("{session}-input")).unwrap();
        let expected = GamepadFeedback {
            slot: GamepadSlot::new(1).unwrap(),
            low_frequency: 1_000,
            high_frequency: 2_000,
            duration_ms: 0,
        };
        client
            .control
            .send(Message::GamepadFeedback(expected))
            .unwrap();
        assert_eq!(
            feedback.recv_timeout(Duration::from_secs(1)).unwrap(),
            expected
        );
        transport.shutdown();
    }

    #[test]
    fn stalled_client_is_evicted_instead_of_blocking_the_writer() {
        let session = format!(
            "snowglass-runtime-stalled-input-test-{}",
            std::process::id()
        );
        let listener = LocalSocketListener::bind(&session).unwrap();
        let client = LocalSocket::connect(&session).unwrap();
        let server = listener.accept().unwrap();
        server
            .set_write_timeout(Some(CONTROL_WRITE_TIMEOUT))
            .unwrap();

        let clients = Arc::new(Mutex::new(vec![server]));
        let stopping = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::unbounded_blocking();
        let worker = spawn_control_writer(receiver, clients.clone(), stopping).unwrap();

        // Fill the socket buffer while the client deliberately does not read.
        // Once the bounded send expires, the writer must remove that client so
        // the remaining queued messages and Stop command can drain promptly.
        for _ in 0..32_768 {
            sender
                .send(ControlCommand::Send(Message::Shutdown))
                .unwrap();
        }
        sender.send(ControlCommand::Stop).unwrap();

        let started = Instant::now();
        while !worker.is_finished() && started.elapsed() < Duration::from_secs(2) {
            thread::sleep(Duration::from_millis(10));
        }
        let finished_without_peer = worker.is_finished();
        drop(client);
        worker.join().unwrap();
        listener.close();

        assert!(
            finished_without_peer,
            "control writer remained blocked on a non-reading client"
        );
        assert!(clients.lock().is_empty());
    }
}
