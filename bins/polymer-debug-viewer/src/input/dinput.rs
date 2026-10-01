use parking_lot::RwLock;
use polymer_ipc::{Message, MessageSink};
use polymer_renderer::{
    CompositingPlan, Presentation,
    input::{PointerEventSink, PointerRoutingState},
};
use std::sync::Arc;

#[derive(Clone, Default)]
pub(crate) struct InputEndpoint(Arc<RwLock<Option<polymer_ipc::input::host::InputIpcListener>>>);

impl InputEndpoint {
    pub fn attach(&self, instance: &polymer_runtime::EmulatorInstance) {
        *self.0.write() = Some(instance.input());
    }

    pub fn detach(&self) {
        *self.0.write() = None;
    }
}

impl MessageSink for InputEndpoint {
    fn send_message(&self, message: Message) -> bool {
        self.0
            .read()
            .as_ref()
            .is_some_and(|sender| sender.send_control(message))
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cancel_pointer_connection(
    connection: u64,
    routing: &mut PointerRoutingState,
    presentation: &Presentation,
    plan: &CompositingPlan,
    sender: &impl PointerEventSink,
) {
    routing.cancel(connection, presentation, plan, sender);
}
