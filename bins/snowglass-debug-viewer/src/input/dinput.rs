use parking_lot::RwLock;
use snowglass_ipc::{Message, MessageSink};
use snowglass_renderer::{
    CompositingPlan, Presentation,
    input::{PointerEventSink, PointerRoutingState},
};
use std::sync::Arc;

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

#[allow(clippy::too_many_arguments)]
pub(crate) fn cancel_pointer_connection(
    connection: u64,
    routing: &mut PointerRoutingState,
    presentation: &Presentation,
    plan: &CompositingPlan,
    sender: &impl PointerEventSink,
    direct_input: &mut DirectInputForwarder,
) {
    let Some(buttons) = routing.cancel(connection, presentation, plan, sender) else {
        return;
    };
    direct_input.release_mouse_buttons(buttons);
}
