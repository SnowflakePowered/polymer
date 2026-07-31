use std::sync::Arc;

use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use serde_json::Value;

use crate::api::LaunchOptions;
use snowglass_input::hub::InputTransaction;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct InstanceToken(pub u32);

#[derive(Debug)]
pub enum EmulatorCommand {
    Launch {
        token: InstanceToken,
        options: LaunchOptions,
    },
    Close(InstanceToken),
    SetVolume {
        token: InstanceToken,
        volume: f64,
    },
    SetMuted {
        token: InstanceToken,
        muted: bool,
    },
    Message {
        token: InstanceToken,
        name: String,
        args: Value,
    },
    Input {
        token: InstanceToken,
        active: bool,
    },
    ConfigureInput {
        request_id: u32,
        transaction: InputTransaction,
    },
}

#[derive(Clone)]
pub struct EmulatorCommandSink(Arc<dyn Fn(EmulatorCommand) + Send + Sync>);

impl EmulatorCommandSink {
    pub fn new(send: impl Fn(EmulatorCommand) + Send + Sync + 'static) -> Self {
        Self(Arc::new(send))
    }

    fn send(&self, command: EmulatorCommand) {
        (self.0)(command);
    }
}

impl Default for EmulatorCommandSink {
    fn default() -> Self {
        Self::new(|_| {})
    }
}

pub(super) struct EmulatorBridge {
    pub command_sink: EmulatorCommandSink,
    next_token: u32,
}

impl Default for EmulatorBridge {
    fn default() -> Self {
        Self {
            command_sink: EmulatorCommandSink::default(),
            next_token: 0,
        }
    }
}

#[op2]
pub(super) fn op_emulator_launch(
    state: &mut OpState,
    #[serde] options: LaunchOptions,
) -> Result<u32, JsErrorBox> {
    options.validate().map_err(JsErrorBox::type_error)?;
    let emulator = state.borrow_mut::<EmulatorBridge>();
    emulator.next_token = emulator.next_token.wrapping_add(1).max(1);
    let token = InstanceToken(emulator.next_token);
    emulator
        .command_sink
        .send(EmulatorCommand::Launch { token, options });
    Ok(token.0)
}

#[op2(fast)]
pub(super) fn op_emulator_close(state: &mut OpState, token: u32) {
    state
        .borrow::<EmulatorBridge>()
        .command_sink
        .send(EmulatorCommand::Close(InstanceToken(token)));
}

#[op2(fast)]
pub(super) fn op_emulator_set_volume(state: &mut OpState, token: u32, volume: f64) {
    state
        .borrow::<EmulatorBridge>()
        .command_sink
        .send(EmulatorCommand::SetVolume {
            token: InstanceToken(token),
            volume,
        });
}

#[op2(fast)]
pub(super) fn op_emulator_set_muted(state: &mut OpState, token: u32, muted: bool) {
    state
        .borrow::<EmulatorBridge>()
        .command_sink
        .send(EmulatorCommand::SetMuted {
            token: InstanceToken(token),
            muted,
        });
}

#[op2]
pub(super) fn op_emulator_message(
    state: &mut OpState,
    token: u32,
    #[string] name: String,
    #[serde] args: serde_json::Value,
) {
    state
        .borrow::<EmulatorBridge>()
        .command_sink
        .send(EmulatorCommand::Message {
            token: InstanceToken(token),
            name,
            args,
        });
}

#[op2(fast)]
pub(super) fn op_emulator_input(state: &mut OpState, token: u32, active: bool) {
    state
        .borrow::<EmulatorBridge>()
        .command_sink
        .send(EmulatorCommand::Input {
            token: InstanceToken(token),
            active,
        });
}

#[op2]
pub(super) fn op_input_configure(
    state: &mut OpState,
    request_id: u32,
    #[serde] transaction: InputTransaction,
) {
    state
        .borrow::<EmulatorBridge>()
        .command_sink
        .send(EmulatorCommand::ConfigureInput {
            request_id,
            transaction,
        });
}
