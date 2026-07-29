use deno_core::{OpState, op2};
use serde_json::Value;

use super::state::BridgeState;

#[derive(Debug)]
pub enum Command {
    Launch {
        instance_id: String,
        options: Value,
    },
    Close(String),
    SetVolume {
        instance_id: String,
        volume: f64,
    },
    SetMuted {
        instance_id: String,
        muted: bool,
    },
    Message {
        instance_id: String,
        name: String,
        args: Value,
    },
    Input(bool),
}

#[op2]
#[string]
pub(super) fn op_game_launch(state: &mut OpState, #[serde] options: serde_json::Value) -> String {
    let state = state.borrow_mut::<BridgeState>();
    state.next_instance += 1;
    let instance_id = state.next_instance.to_string();
    state.commands.push(Command::Launch {
        instance_id: instance_id.clone(),
        options,
    });
    instance_id
}

#[op2(fast)]
pub(super) fn op_game_close(state: &mut OpState, #[string] instance_id: String) {
    state
        .borrow_mut::<BridgeState>()
        .commands
        .push(Command::Close(instance_id));
}

#[op2(fast)]
pub(super) fn op_game_set_volume(state: &mut OpState, #[string] instance_id: String, volume: f64) {
    state
        .borrow_mut::<BridgeState>()
        .commands
        .push(Command::SetVolume {
            instance_id,
            volume,
        });
}

#[op2(fast)]
pub(super) fn op_game_set_muted(state: &mut OpState, #[string] instance_id: String, muted: bool) {
    state
        .borrow_mut::<BridgeState>()
        .commands
        .push(Command::SetMuted { instance_id, muted });
}

#[op2]
pub(super) fn op_game_message(
    state: &mut OpState,
    #[string] instance_id: String,
    #[string] name: String,
    #[serde] args: serde_json::Value,
) {
    state
        .borrow_mut::<BridgeState>()
        .commands
        .push(Command::Message {
            instance_id,
            name,
            args,
        });
}

#[op2(fast)]
pub(super) fn op_game_input(state: &mut OpState, active: bool) {
    state
        .borrow_mut::<BridgeState>()
        .commands
        .push(Command::Input(active));
}
