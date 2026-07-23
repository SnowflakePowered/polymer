use super::pointer::{DisplayedSurface, PointerRoutingState, PointerTarget};
use snowglass_input::gamepad::{GamepadUpdate, LatestGamepadMailbox};
use snowglass_input::mouse::layout::Extent;
use snowglass_ipc::{LocalSocket, LocalSocketListener, Message};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

pub(crate) use snowglass_input_dinput::{DirectInputForwarder, RawKeyboardForwarder};

struct InputClients {
    sockets: Vec<LocalSocket>,
    gamepads: Vec<Option<GamepadUpdate>>,
}

#[derive(Clone, Default)]
pub(crate) struct InputLatencyStats(Arc<Mutex<VecDeque<Duration>>>);

#[derive(Clone, Copy, Default)]
pub(crate) struct InputLatencySummary {
    pub samples: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

impl InputLatencyStats {
    fn record(&self, latency: Duration) {
        let mut samples = self.0.lock().unwrap_or_else(|error| error.into_inner());
        samples.push_back(latency);
        if samples.len() > 1024 {
            samples.pop_front();
        }
    }

    pub fn summary(&self) -> InputLatencySummary {
        let samples = self.0.lock().unwrap_or_else(|error| error.into_inner());
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
        InputLatencySummary {
            samples: values.len(),
            p50_ms: percentile(0.50),
            p95_ms: percentile(0.95),
            p99_ms: percentile(0.99),
        }
    }
}

pub(crate) fn spawn_input_ipc(
    listener: LocalSocketListener,
    gamepad_mailbox: Arc<LatestGamepadMailbox>,
) -> (mpsc::Sender<Message>, InputLatencyStats) {
    let (sender, input) = mpsc::channel();
    let clients = Arc::new(Mutex::new(InputClients {
        sockets: Vec::new(),
        gamepads: vec![None; 256],
    }));
    let accepted_clients = clients.clone();
    thread::spawn(move || {
        loop {
            let socket = match listener.accept() {
                Ok(socket) => {
                    eprintln!("snowglass: target input IPC connected");
                    socket
                }
                Err(error) => {
                    eprintln!("snowglass: input IPC accept ended: {error}");
                    return;
                }
            };
            let mut socket = socket;
            let mut alive = true;
            let mut clients = accepted_clients
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            for update in clients.gamepads.iter().flatten().copied() {
                if socket.send(Message::GamepadUpdate(update)).is_err() {
                    alive = false;
                    break;
                }
            }
            if alive {
                clients.sockets.push(socket);
            }
        }
    });
    let gamepad_clients = clients.clone();
    let latency = InputLatencyStats::default();
    let gamepad_latency = latency.clone();
    thread::spawn(move || {
        loop {
            let updates = gamepad_mailbox.wait_and_drain();
            for stamped in updates {
                let update = stamped.update;
                let mut clients = gamepad_clients
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                clients.gamepads[update.slot.index()] = Some(update);
                clients
                    .sockets
                    .retain_mut(|socket| socket.send(Message::GamepadUpdate(update)).is_ok());
                if !clients.sockets.is_empty() {
                    gamepad_latency.record(stamped.sampled_at.elapsed());
                }
            }
        }
    });
    thread::spawn(move || {
        while let Ok(event) = input.recv() {
            clients
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .sockets
                .retain_mut(|socket| match socket.send(event) {
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
    });
    (sender, latency)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cancel_pointer_connection(
    connection: u64,
    routing: &mut PointerRoutingState,
    viewer_extent: Extent,
    surfaces: &[DisplayedSurface],
    targets: &HashMap<u64, PointerTarget>,
    sender: &mpsc::Sender<Message>,
    direct_input: &mut DirectInputForwarder,
) {
    let Some(buttons) = routing.cancel(connection, viewer_extent, surfaces, targets, sender) else {
        return;
    };
    direct_input.release_mouse_buttons(buttons);
}
