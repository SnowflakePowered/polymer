use super::InputActor;
use actix::{AtomicResponse, Context, Handler, WrapFuture};
use polymer_runtime::frontend::input::{Configure, ControllerApi};
use serde_json::Value;

impl Handler<Configure> for InputActor {
    type Result = AtomicResponse<Self, ()>;
    fn handle(&mut self, message: Configure, _: &mut Context<Self>) -> Self::Result {
        let state = self.state.clone();
        AtomicResponse::new(Box::pin(
            async move {
                if let Ok(change) =
                    state.with(|state| state.configure(message.request_id, message.transaction))
                {
                    let _ = state.finish_controller_change(change).await;
                }
            }
            .into_actor(self),
        ))
    }
}

impl Handler<ControllerApi> for InputActor {
    type Result = AtomicResponse<Self, Result<Value, String>>;
    fn handle(&mut self, message: ControllerApi, _: &mut Context<Self>) -> Self::Result {
        let state = self.state.clone();
        AtomicResponse::new(Box::pin(
            async move {
                let command =
                    state.with(|state| state.input.begin_controller_command(message.command))?;
                let outcome = command.await?;
                let change = state.with(|state| {
                    let outcome = state.input.finish_controller_command(outcome)?;
                    state.controller_api_response(outcome)
                })?;
                state.finish_controller_change(change).await
            }
            .into_actor(self),
        ))
    }
}
