//! Main-thread command actor sharing the coordinator with native callbacks.

mod emulator;
mod input;
mod state;
mod webrtc;

use super::{ControllerChange, ShellCoordinator, UserEvent};
use actix::{Actor, ActorContext, Addr, Context, Handler, Message};
use polymer_runtime::{FrontendCommands, frontend};
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
    sync::Arc,
};

#[derive(Clone)]
struct GuiState(Weak<RefCell<Option<ShellCoordinator>>>);

impl GuiState {
    fn with<R>(
        &self,
        operation: impl FnOnce(&mut ShellCoordinator) -> Result<R, String>,
    ) -> Result<R, String> {
        let state = self
            .0
            .upgrade()
            .ok_or_else(|| "GUI host is unavailable".to_owned())?;
        let mut state = state.borrow_mut();
        operation(
            state
                .as_mut()
                .ok_or_else(|| "GUI host is unavailable".to_owned())?,
        )
    }

    async fn finish_controller_change(
        &self,
        change: ControllerChange,
    ) -> Result<serde_json::Value, String> {
        match change {
            ControllerChange::Ready(value) => Ok(value),
            ControllerChange::Pending {
                transaction,
                completion,
                response,
            } => {
                let callback = response
                    .await
                    .map_err(|_| {
                        "controller plugin session closed during topology change".to_owned()
                    })
                    .and_then(|result| result);
                self.with(move |state| {
                    let result = state
                        .input
                        .finish_controller_transaction(*transaction, callback);
                    state.complete_controller_transaction(completion, result)
                })
            }
        }
    }
}

pub(super) struct GuiActors {
    control: Addr<GuiActor>,
    input: Addr<InputActor>,
    state: Addr<StateActor>,
}

// Each of these mailboxes waits for its own transaction, never the GUI as a whole.
struct InputActor {
    state: GuiState,
}
struct StateActor {
    state: GuiState,
}
pub(super) struct GuiActor {
    state: GuiState,
}

impl GuiActors {
    pub(super) fn start_for(state: &Rc<RefCell<Option<ShellCoordinator>>>) -> Self {
        let state = GuiState(Rc::downgrade(state));
        Self {
            control: GuiActor {
                state: state.clone(),
            }
            .start(),
            input: InputActor {
                state: state.clone(),
            }
            .start(),
            state: StateActor { state }.start(),
        }
    }

    pub(super) fn close(&self, token: crate::shell_runtime::InstanceToken) {
        self.state.do_send(frontend::emulator::Close { token });
    }

    pub(super) fn shutdown(&self) {
        self.control.do_send(Shutdown);
        self.input.do_send(Shutdown);
        self.state.do_send(Shutdown);
    }

    pub(super) fn commands(
        &self,
        proxy: crate::window_manager::EventLoopProxy<UserEvent>,
    ) -> FrontendCommands {
        FrontendCommands {
            emulator_launch: Some(self.control.clone().recipient()),
            emulator_close: Some(self.state.clone().recipient()),
            emulator_set_volume: Some(self.control.clone().recipient()),
            emulator_set_muted: Some(self.control.clone().recipient()),
            emulator_message: Some(self.control.clone().recipient()),
            emulator_input: Some(self.control.clone().recipient()),
            emulator_pause_state: Some(self.control.clone().recipient()),
            emulator_save_state: Some(self.state.clone().recipient()),
            emulator_resume_state: Some(self.state.clone().recipient()),
            emulator_state_screenshot: Some(self.control.clone().recipient()),
            input_configure: Some(self.input.clone().recipient()),
            input_controller_api: Some(self.input.clone().recipient()),
            webrtc_start: Some(self.control.clone().recipient()),
            webrtc_stop: Some(self.control.clone().recipient()),
            wake: Some(Arc::new(move || {
                let _ = proxy.send_event(UserEvent::WakeActors);
            })),
        }
    }
}

impl Actor for GuiActor {
    type Context = Context<Self>;
}

#[derive(Message)]
#[rtype(result = "()")]
pub(super) struct Shutdown;

impl Handler<Shutdown> for GuiActor {
    type Result = ();
    fn handle(&mut self, _: Shutdown, ctx: &mut Context<Self>) {
        ctx.stop();
    }
}

impl Actor for InputActor {
    type Context = Context<Self>;
}
impl Actor for StateActor {
    type Context = Context<Self>;
}

impl Handler<Shutdown> for InputActor {
    type Result = ();
    fn handle(&mut self, _: Shutdown, ctx: &mut Context<Self>) {
        ctx.stop();
    }
}
impl Handler<Shutdown> for StateActor {
    type Result = ();
    fn handle(&mut self, _: Shutdown, ctx: &mut Context<Self>) {
        ctx.stop();
    }
}
