use parking_lot::RwLock;
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
