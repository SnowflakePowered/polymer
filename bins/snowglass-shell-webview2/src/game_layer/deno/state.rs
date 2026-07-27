use deno_core::{OpState, op2};
use serde_json::Value;

use super::{dom::Mutation, game::Command, input::EventResult};

#[derive(Default)]
pub(super) struct BridgeState {
    pub mutations: Vec<Mutation>,
    pub messages: Vec<Value>,
    pub proxy_messages: Vec<Value>,
    pub event: EventResult,
    pub commands: Vec<Command>,
    pub next_instance: u64,
}

#[op2]
pub(super) fn op_post_message(state: &mut OpState, #[serde] message: serde_json::Value) {
    state.borrow_mut::<BridgeState>().messages.push(message);
}

#[op2]
pub(super) fn op_proxy_message(state: &mut OpState, #[serde] message: serde_json::Value) {
    state
        .borrow_mut::<BridgeState>()
        .proxy_messages
        .push(message);
}
