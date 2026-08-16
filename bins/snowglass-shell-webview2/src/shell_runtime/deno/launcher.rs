use std::sync::Arc;

use deno_core::{OpState, op2};

use crate::{cli::GameLaunchRequest, launcher::LauncherStartRequest};

#[derive(Debug)]
pub enum LauncherCommand {
    Open(GameLaunchRequest),
    Start(LauncherStartRequest),
    Cancel,
    Settle { id: u64, error: Option<String> },
}

#[derive(Clone)]
pub struct LauncherCommandSink(Arc<dyn Fn(LauncherCommand) + Send + Sync>);

impl LauncherCommandSink {
    pub fn new(send: impl Fn(LauncherCommand) + Send + Sync + 'static) -> Self {
        Self(Arc::new(send))
    }

    fn send(&self, command: LauncherCommand) {
        (self.0)(command);
    }
}

impl Default for LauncherCommandSink {
    fn default() -> Self {
        Self::new(|_| {})
    }
}

#[derive(Default)]
pub(super) struct LauncherBridge(pub LauncherCommandSink);

#[op2]
pub(super) fn op_launcher_open(
    state: &mut OpState,
    #[string] path: String,
    #[string] emulator: Option<String>,
) {
    state
        .borrow::<LauncherBridge>()
        .0
        .send(LauncherCommand::Open(GameLaunchRequest {
            game: path.into(),
            emulator,
        }));
}

#[op2]
pub(super) fn op_launcher_start(state: &mut OpState, #[serde] request: LauncherStartRequest) {
    state
        .borrow::<LauncherBridge>()
        .0
        .send(LauncherCommand::Start(request));
}

#[op2(fast)]
pub(super) fn op_launcher_cancel(state: &mut OpState) {
    state
        .borrow::<LauncherBridge>()
        .0
        .send(LauncherCommand::Cancel);
}

#[op2]
pub(super) fn op_launcher_settle(
    state: &mut OpState,
    #[bigint] id: u64,
    #[string] error: Option<String>,
) {
    state
        .borrow::<LauncherBridge>()
        .0
        .send(LauncherCommand::Settle { id, error });
}
