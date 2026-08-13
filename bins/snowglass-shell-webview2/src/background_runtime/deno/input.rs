use deno_core::{OpState, op2};
use serde::Serialize;

use super::state::BridgeState;

#[derive(Clone, Copy, Debug, Default)]
pub struct EventResult {
    pub handled: bool,
    pub default_prevented: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MouseEvent {
    pub client_x: f32,
    pub client_y: f32,
    pub button: u8,
    pub buttons: u8,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub alt_key: bool,
    pub meta_key: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyboardEvent {
    pub key: String,
    pub code: String,
    pub repeat: bool,
    pub alt_key: bool,
    pub ctrl_key: bool,
    pub shift_key: bool,
    pub meta_key: bool,
}

#[op2(fast)]
pub(super) fn op_event_result(state: &mut OpState, handled: bool, default_prevented: bool) {
    state.borrow_mut::<BridgeState>().event = EventResult {
        handled,
        default_prevented,
    };
}
