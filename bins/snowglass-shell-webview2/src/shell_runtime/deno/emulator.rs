use std::{cell::RefCell, rc::Rc, sync::Arc};

use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use serde_json::Value;

use crate::launch::AttachedLaunchPlan;
use snowglass_input::hub::InputTransaction;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct InstanceToken(pub u32);

#[derive(Debug)]
pub enum EmulatorCommand {
    Launch {
        token: InstanceToken,
        options: AttachedLaunchPlan,
        result: tokio::sync::oneshot::Sender<Result<(), String>>,
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
}

impl Default for EmulatorBridge {
    fn default() -> Self {
        Self {
            command_sink: EmulatorCommandSink::default(),
        }
    }
}

#[op2]
pub(super) async fn op_emulator_launch(
    state: Rc<RefCell<OpState>>,
    token: u32,
    #[serde] options: AttachedLaunchPlan,
) -> Result<(), JsErrorBox> {
    options.validate().map_err(JsErrorBox::type_error)?;
    let command_sink = state
        .borrow()
        .borrow::<EmulatorBridge>()
        .command_sink
        .clone();
    let (result, response) = tokio::sync::oneshot::channel();
    command_sink.send(EmulatorCommand::Launch {
        token: InstanceToken(token),
        options,
        result,
    });
    response
        .await
        .map_err(|_| JsErrorBox::generic("emulator launch coordinator is unavailable"))?
        .map_err(JsErrorBox::generic)
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
