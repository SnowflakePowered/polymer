//! Viewer-to-target input transport using the existing Snowglass IPC paths.

use crossfire::{MTx, Rx, mpsc};
use parking_lot::Mutex;
use snowglass_input::gamepad::{GamepadUpdate, LatestGamepadMailbox};
use snowglass_ipc::{LocalSocket, LocalSocketListener, Message, input::InputPublisher};
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
    listener: LocalSocketListener,
    stopping: Arc<AtomicBool>,
    clients: Arc<Mutex<Vec<LocalSocket>>>,
    pending_socket: Arc<Mutex<Option<LocalSocket>>>,
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
        let (control, receiver) = mpsc::unbounded_blocking();

        let acceptor = spawn_acceptor(
            listener.clone(),
            gamepads.clone(),
            clients.clone(),
            pending_socket.clone(),
            stopping.clone(),
        );
        let gamepad = spawn_gamepad_writer(mailbox.clone(), gamepads, stopping.clone());
        let control_worker = spawn_control_writer(receiver, clients.clone());
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
            listener,
            stopping,
            clients,
            pending_socket,
            workers: Mutex::new(Some(workers)),
        })))
    }

    pub(crate) fn send(&self, message: Message) -> bool {
        self.0.control.send(ControlCommand::Send(message)).is_ok()
    }

    pub(crate) fn sender(&self) -> ProtocolInputSender {
        ProtocolInputSender(self.0.control.clone())
    }

    pub(crate) fn publish_gamepad(&self, update: GamepadUpdate) -> std::io::Result<()> {
        self.0.mailbox.publish(update);
        Ok(())
    }

    pub(crate) fn gamepad_mailbox(&self) -> Arc<LatestGamepadMailbox> {
        self.0.mailbox.clone()
    }

    pub(crate) fn shutdown(&self) {
        self.0.shutdown();
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
        let _ = workers.control.join();
        if let Some(socket) = self.pending_socket.lock().as_ref() {
            let _ = socket.shutdown();
        }
        for socket in self.clients.lock().iter() {
            let _ = socket.shutdown();
        }
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
    thread::Builder::new()
        .name("snowglass-runtime-gamepad-send".to_owned())
        .spawn(move || {
            loop {
                for update in mailbox.wait_and_drain() {
                    if stopping.load(Ordering::Acquire) {
                        return;
                    }
                    let _ = gamepads.lock().publish(update.update);
                }
            }
        })
}

fn spawn_acceptor(
    listener: LocalSocketListener,
    gamepads: Arc<Mutex<InputPublisher>>,
    clients: Arc<Mutex<Vec<LocalSocket>>>,
    pending_socket: Arc<Mutex<Option<LocalSocket>>>,
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
                            eprintln!("snowglass: input IPC accept ended: {error}");
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
                    clients.lock().push(socket);
                } else if stopping.load(Ordering::Acquire) {
                    return;
                }
            }
        })
}

fn spawn_control_writer(
    receiver: ControlRx,
    clients: Arc<Mutex<Vec<LocalSocket>>>,
) -> std::io::Result<JoinHandle<()>> {
    thread::Builder::new()
        .name("snowglass-runtime-input-send".to_owned())
        .spawn(move || {
            while let Ok(command) = receiver.recv() {
                let ControlCommand::Send(message) = command else {
                    return;
                };
                clients
                    .lock()
                    .retain_mut(|socket| match socket.send(message) {
                        Ok(()) => true,
                        Err(error) => {
                            if error.kind() != std::io::ErrorKind::BrokenPipe
                                && error.kind() != std::io::ErrorKind::ConnectionReset
                            {
                                eprintln!("snowglass: input IPC send failed: {error}");
                            }
                            false
                        }
                    });
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_joins_workers_and_releases_listener() {
        let session = format!("snowglass-runtime-input-test-{}", std::process::id());
        let transport = InputTransport::bind(&session).unwrap();
        transport.shutdown();
        let replacement = LocalSocketListener::bind(&format!("{session}-input")).unwrap();
        replacement.close();
    }
}
