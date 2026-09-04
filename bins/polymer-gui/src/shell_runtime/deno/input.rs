use super::state::BridgeState;
use deno_core::{OpState, op2};

#[derive(Clone, Copy, Debug, Default)]
pub struct EventResult {
    pub handled: bool,
    pub default_prevented: bool,
    pub propagation_stopped: bool,
}

#[op2(fast)]
pub(super) fn op_event_result(
    state: &mut OpState,
    handled: bool,
    default_prevented: bool,
    propagation_stopped: bool,
) {
    state.borrow_mut::<BridgeState>().event = EventResult {
        handled,
        default_prevented,
        propagation_stopped,
    };
}
