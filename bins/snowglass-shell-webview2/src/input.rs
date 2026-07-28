//! Shell policy for physical input capture.
//!
//! The emulator instance owns IPC. This adapter only gives long-lived shell
//! input objects a stable endpoint while game instances are replaced.

use parking_lot::{Mutex, RwLock};
use snowglass_input::provider::sdl3::{ProviderConfig, Sdl3Provider};
use snowglass_ipc::{Message, MessageSink};
use std::{collections::VecDeque, sync::Arc, time::Duration};

#[derive(Clone, Default)]
pub struct RoutedInputSender(Arc<RwLock<Option<snowglass_runtime::ProtocolInputSender>>>);

impl RoutedInputSender {
    fn attach(&self, instance: &snowglass_runtime::EmulatorInstance) {
        *self.0.write() = Some(instance.protocol_input_sender());
    }

    fn detach(&self) {
        *self.0.write() = None;
    }
}

impl MessageSink for RoutedInputSender {
    fn send_message(&self, message: Message) -> bool {
        self.0
            .read()
            .as_ref()
            .is_some_and(|sender| sender.send(message))
    }
}

pub struct InputTransport {
    control: RoutedInputSender,
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
    pub fn runtime() -> Self {
        let control = RoutedInputSender::default();

        Self {
            control,
            dispatch_latency: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn attach(&self, instance: &snowglass_runtime::EmulatorInstance) {
        self.control.attach(instance);
    }

    pub fn detach(&self) {
        self.control.detach();
    }

    pub fn start_provider(
        &self,
        config: ProviderConfig,
        instance: &snowglass_runtime::EmulatorInstance,
    ) -> Result<Sdl3Provider, String> {
        Sdl3Provider::start(config, instance.gamepad_mailbox())
    }

    pub fn control_sender(&self) -> RoutedInputSender {
        self.control.clone()
    }

    pub fn dispatch_summary(&self) -> DispatchSummary {
        let samples = self.dispatch_latency.lock();
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
