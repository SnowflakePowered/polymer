use super::GuiActor;
use actix::{Context, Handler};
use polymer_runtime::frontend::webrtc::{Start, Stop};

impl Handler<Start> for GuiActor {
    type Result = Result<(), String>;
    fn handle(&mut self, message: Start, _: &mut Context<Self>) -> Self::Result {
        self.state.with(|state| {
            state
                .game_host
                .start_webrtc(message.token, message.bind, &message.targets)
        })
    }
}

impl Handler<Stop> for GuiActor {
    type Result = Result<(), String>;
    fn handle(&mut self, message: Stop, _: &mut Context<Self>) -> Self::Result {
        self.state.with(|state| {
            if state.game_host.stop_webrtc(message.token) {
                Ok(())
            } else {
                Err("the WebRTC publication is not active".to_owned())
            }
        })
    }
}
