//! Save and restore remain sequential through plugin completion.
use super::StateActor;
use actix::{AtomicResponse, Context, Handler, WrapFuture};
use polymer_runtime::frontend::emulator::{ChangeDisc, Close, StateAction};

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

impl Handler<StateAction> for StateActor {
    type Result = AtomicResponse<Self, Result<(), String>>;
    fn handle(&mut self, message: StateAction, _: &mut Context<Self>) -> Self::Result {
        let state = self.state.clone();
        AtomicResponse::new(Box::pin(
            async move {
                let response = state.with_active(message.token, |state| {
                    state
                        .game_host
                        .state_action(message.slot, message.save, message.timeout)
                })?;
                response.await.map_err(|_| {
                    "emulator plugin session closed during save-state action".to_owned()
                })?
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
