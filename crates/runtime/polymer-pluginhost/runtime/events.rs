use deno_core::{OpState, op2};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::mpsc;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static EVENTS: OnceLock<Mutex<EventBus>> = OnceLock::new();

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginEvent {
    pub source_id: u64,
    pub plugin_id: String,
    pub name: String,
    pub detail: Value,
}

#[derive(Default)]
struct EventBus {
    latest: VecDeque<PluginEvent>,
    subscribers: Vec<mpsc::UnboundedSender<PluginEvent>>,
}

impl PluginEvent {
    /// Replays the latest event per source and topic before delivering live events.
    pub fn subscribe() -> mpsc::UnboundedReceiver<Self> {
        let (sender, receiver) = mpsc::unbounded_channel();
        let mut bus = EVENTS.get_or_init(Default::default).lock();
        for event in &bus.latest {
            let _ = sender.send(event.clone());
        }
        bus.subscribers.push(sender);
        receiver
    }
}

pub(super) struct EventEmitter {
    id: u64,
    plugin: String,
}

impl EventEmitter {
    pub fn new(plugin: &str) -> Self {
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            plugin: plugin.to_owned(),
        }
    }

    pub fn publish(&self, name: &str, detail: Value) {
        let event = PluginEvent {
            source_id: self.id,
            plugin_id: self.plugin.clone(),
            name: name.to_owned(),
            detail,
        };
        tracing::info!(plugin = %self.plugin, event = name, detail = %event.detail, "plugin event");
        let mut bus = EVENTS.get_or_init(Default::default).lock();
        bus.latest
            .retain(|previous| previous.source_id != self.id || previous.name != name);
        bus.latest.push_back(event.clone());
        if bus.latest.len() > 256 {
            bus.latest.pop_front();
        }
        bus.subscribers
            .retain(|subscriber| subscriber.send(event.clone()).is_ok());
    }

    pub fn scope(self: &Arc<Self>, name: &str, initial: Value, interrupted: Value) -> EventScope {
        self.publish(name, initial);
        EventScope {
            emitter: self.clone(),
            name: name.to_owned(),
            terminal: interrupted,
        }
    }
}

pub(super) struct EventScope {
    emitter: Arc<EventEmitter>,
    name: String,
    terminal: Value,
}

impl EventScope {
    pub fn finish(mut self, detail: Value) {
        self.terminal = detail;
    }
}

impl Drop for EventScope {
    fn drop(&mut self) {
        self.emitter.publish(&self.name, self.terminal.take());
    }
}

#[op2]
pub(super) fn op_plugin_emit_event(
    state: &mut OpState,
    #[string] name: String,
    #[serde] detail: serde_json::Value,
) {
    state.borrow::<Arc<EventEmitter>>().publish(&name, detail);
}
