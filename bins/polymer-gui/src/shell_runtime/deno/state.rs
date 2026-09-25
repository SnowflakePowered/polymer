use deno_core::{OpState, op2};
use serde_json::Value;

use super::{dom::LayoutSnapshot, input::EventResult};

#[derive(Default)]
pub(super) struct BridgeState {
    pub messages: Vec<Value>,
    pub event: EventResult,
    pub layout: Option<LayoutSnapshot>,
}

#[op2]
pub(super) fn op_post_message(state: &mut OpState, #[serde] message: serde_json::Value) {
    state.borrow_mut::<BridgeState>().messages.push(message);
}
