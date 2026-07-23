use super::pointer::PointerRoutingState;
use snowglass_input::gamepad::LatestGamepadMailbox;
use snowglass_ipc::{LocalSocket, LocalSocketListener, Message, input::InputPublisher};
use snowglass_renderer::Presentation;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

pub(crate) use snowglass_input_dinput::{DirectInputForwarder, RawKeyboardForwarder};

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
    let gamepads = Arc::new(Mutex::new(
        InputPublisher::new().expect("create shared input transport"),
    ));
    let clients = Arc::new(Mutex::new(Vec::<LocalSocket>::new()));
    let client_count = Arc::new(AtomicUsize::new(0));
    let accepted_clients = clients.clone();
    let accepted_gamepads = gamepads.clone();
    let accepted_count = client_count.clone();
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
            let _ = socket.set_read_timeout(Some(Duration::from_secs(1)));
            let accepted = accepted_gamepads
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .accept(&mut socket)
                .is_ok();
            if accepted {
                let _ = socket.set_read_timeout(None);
                let mut clients = accepted_clients
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                clients.push(socket);
                accepted_count.store(clients.len(), Ordering::Release);
            }
        }
    });
    let latency = InputLatencyStats::default();
    let gamepad_latency = latency.clone();
    let gamepad_count = client_count.clone();
    thread::spawn(move || {
        loop {
            let updates = gamepad_mailbox.wait_and_drain();
            for stamped in updates {
                let update = stamped.update;
                let published = gamepads
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .publish(update)
                    .is_ok();
                if gamepad_count.load(Ordering::Acquire) != 0 {
                    if published {
                        gamepad_latency.record(stamped.sampled_at.elapsed());
                    }
                }
            }
        }
    });
    thread::spawn(move || {
        while let Ok(event) = input.recv() {
            let mut clients = clients.lock().unwrap_or_else(|error| error.into_inner());
            clients.retain_mut(|socket| match socket.send(event) {
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
            client_count.store(clients.len(), Ordering::Release);
        }
    });
    (sender, latency)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cancel_pointer_connection(
    connection: u64,
    routing: &mut PointerRoutingState,
    presentation: &Presentation,
    sender: &mpsc::Sender<Message>,
    direct_input: &mut DirectInputForwarder,
) {
    let Some(buttons) = routing.cancel(connection, presentation, sender) else {
        return;
    };
    direct_input.release_mouse_buttons(buttons);
}
