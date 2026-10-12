use blitz_dom::NodeId;
use deno_core::{OpState, op2};

use super::dom::ScriptNodeId;
use crate::shell::document::{MediaState, SharedShellDocument};

#[op2]
#[serde]
pub(super) fn op_media_state(state: &OpState, node: f64) -> Option<MediaState> {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.media().state(NodeId::from_script(node))
}

#[op2(fast)]
pub(super) fn op_media_load(state: &OpState, node: f64) -> bool {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.load_media(NodeId::from_script(node))
}

#[op2(fast)]
pub(super) fn op_media_play(state: &OpState, node: f64) {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.media().play(NodeId::from_script(node));
}

#[op2(fast)]
pub(super) fn op_media_pause(state: &OpState, node: f64) -> f64 {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.media().pause(NodeId::from_script(node))
}

#[op2(fast)]
pub(super) fn op_media_seek(state: &OpState, node: f64, seconds: f64) -> f64 {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.media().seek(NodeId::from_script(node), seconds)
}

#[op2(fast)]
pub(super) fn op_media_configure(state: &OpState, node: f64, volume: f32, muted: bool) {
    let shared = state.borrow::<SharedShellDocument>();
    let mut shell = shared.0.borrow_mut();
    shell.configure_media(NodeId::from_script(node), volume, muted);
}
