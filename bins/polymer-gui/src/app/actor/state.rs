//! Save and restore remain sequential through plugin completion and metadata commit.
use super::StateActor;
use crate::shell_runtime::InstanceToken;
use actix::{AtomicResponse, Context, Handler, WrapFuture};
use polymer_runtime::frontend::emulator::{ChangeDisc, Close, ResumeState, SaveState};
use serde_json::Value;
use std::time::Duration;

impl Handler<ChangeDisc> for StateActor {
    type Result = AtomicResponse<Self, Result<(), String>>;
    fn handle(&mut self, message: ChangeDisc, _: &mut Context<Self>) -> Self::Result {
        let state = self.state.clone();
        AtomicResponse::new(Box::pin(
            async move {
                let response = state.with_active(message.token, |state| {
                    state.game_host.change_disc(message.index)
                })?;
                response.await.map_err(|_| {
                    "emulator plugin session closed during disc change".to_owned()
                })??;
                state.with_active(message.token, |state| {
                    state.game_host.commit_disc_change(message.index)
                })
            }
            .into_actor(self),
        ))
    }
}

impl Handler<SaveState> for StateActor {
    type Result = AtomicResponse<Self, Result<Value, String>>;
    fn handle(&mut self, message: SaveState, _: &mut Context<Self>) -> Self::Result {
        self.state_action(message.token, None, message.timeout)
    }
}

impl Handler<ResumeState> for StateActor {
    type Result = AtomicResponse<Self, Result<Value, String>>;
    fn handle(&mut self, message: ResumeState, _: &mut Context<Self>) -> Self::Result {
        self.state_action(message.token, Some(message.slot), message.timeout)
    }
}

impl StateActor {
    fn state_action(
        &self,
        token: InstanceToken,
        restore_slot: Option<u32>,
        timeout: Duration,
    ) -> AtomicResponse<Self, Result<Value, String>> {
        let state = self.state.clone();
        AtomicResponse::new(Box::pin(
            async move {
                let (slot, response) = state.with_active(token, |state| match restore_slot {
                    Some(slot) => state
                        .game_host
                        .resume_state(slot, timeout)
                        .map(|response| (slot, response)),
                    None => state.game_host.save_state(timeout),
                })?;
                response.await.map_err(|_| {
                    "emulator plugin session closed during save-state action".to_owned()
                })??;
                state.with_active(token, |state| {
                    let metadata = if restore_slot.is_none() {
                        state.game_host.commit_save_state(slot)?
                    } else {
                        state
                            .game_host
                            .save_state_metadata(slot)
                            .ok_or_else(|| format!("save-state slot {slot} no longer exists"))?
                    };
                    serde_json::to_value(metadata).map_err(|error| error.to_string())
                })
            }
            .into_actor(self),
        ))
    }
}

impl Handler<Close> for StateActor {
    type Result = AtomicResponse<Self, ()>;
    fn handle(&mut self, message: Close, _: &mut Context<Self>) -> Self::Result {
        let state = self.state.clone();
        AtomicResponse::new(Box::pin(
            async move {
                if let Ok(Some((pending, response))) =
                    state.with(|state| Ok(state.begin_quit(message.token)))
                {
                    let callback = response
                        .await
                        .map_err(|_| "emulator plugin session closed during onQuit".to_owned())
                        .and_then(|result| result);
                    let _ = state.with_active(pending.token, |state| {
                        state.finish_quit(pending, callback);
                        Ok(())
                    });
                }
            }
            .into_actor(self),
        ))
    }
}
