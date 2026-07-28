use parking_lot::{Mutex, RwLock};
use snowglass_ipc::{Message, MessageSink};
use snowglass_renderer::{CompositingPlan, Presentation, input::PointerRoutingState};
use std::{collections::VecDeque, sync::Arc, time::Duration};

pub(crate) use snowglass_input_dinput::{DirectInputForwarder, RawKeyboardForwarder};

#[derive(Clone, Default)]
pub(crate) struct RoutedInputSender(Arc<RwLock<Option<snowglass_runtime::ProtocolInputSender>>>);

impl RoutedInputSender {
    pub fn attach(&self, instance: &snowglass_runtime::EmulatorInstance) {
        *self.0.write() = Some(instance.protocol_input_sender());
    }

    pub fn detach(&self) {
        *self.0.write() = None;
    }

    pub fn send(&self, message: Message) -> bool {
        self.send_message(message)
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
    pub fn summary(&self) -> InputLatencySummary {
        let samples = self.0.lock();
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn cancel_pointer_connection(
    connection: u64,
    routing: &mut PointerRoutingState,
    presentation: &Presentation,
    plan: &CompositingPlan,
    sender: &impl MessageSink,
    direct_input: &mut DirectInputForwarder,
) {
    let Some(buttons) = routing.cancel(connection, presentation, plan, sender) else {
        return;
    };
    direct_input.release_mouse_buttons(buttons);
}
