use super::GuiActor;
use crate::{input::InputPriority, instance_events};
use actix::{Context, Handler, ResponseActFuture, WrapFuture};
use polymer_runtime::frontend::emulator::*;
use std::time::Instant;

impl Handler<Launch> for GuiActor {
    type Result = ResponseActFuture<Self, Result<(), String>>;
    fn handle(&mut self, message: Launch, _: &mut Context<Self>) -> Self::Result {
        let validation = self
            .state
            .with(|state| Ok(state.shell.validate_launch(message.options.clone())));
        let state = self.state.clone();
        Box::pin(
            async move {
                let invalid_attachment = validation?.await.unwrap_or(None);
                state.with(|state| {
                    state.launch(
                        message.token,
                        message.options,
                        message.prepared_filesystem,
                        message.controller_session,
                        invalid_attachment,
                    )
                })
            }
            .into_actor(self),
        )
    }
}

impl Handler<SetVolume> for GuiActor {
    type Result = ();
    fn handle(&mut self, message: SetVolume, _: &mut Context<Self>) {
        let _ = self.state.with(|state| {
            if state.game_host.set_volume(message.token, message.volume) {
                state.publish_audio_state(false);
            }
            Ok(())
        });
    }
}

impl Handler<SetMuted> for GuiActor {
    type Result = ();
    fn handle(&mut self, message: SetMuted, _: &mut Context<Self>) {
        let _ = self.state.with(|state| {
            if state.game_host.set_muted(message.token, message.muted) {
                state.publish_audio_state(false);
            }
            Ok(())
        });
    }
}

impl Handler<Message> for GuiActor {
    type Result = ();
    fn handle(&mut self, message: Message, _: &mut Context<Self>) {
        let _ = self.state.with_active(message.token, |state| {
            state.emit_instance_event(
                message.token,
                "message",
                &instance_events::message(&message.name, message.args),
            );
            Ok(())
        });
    }
}

impl Handler<Input> for GuiActor {
    type Result = ();
    fn handle(&mut self, message: Input, _: &mut Context<Self>) {
        let _ = self.state.with_active(message.token, |state| {
            state.set_input_priority(if message.active {
                InputPriority::Game
            } else {
                InputPriority::Ui
            });
            Ok(())
        });
    }
}

impl Handler<PauseState> for GuiActor {
    type Result = ResponseActFuture<Self, Result<(), String>>;
    fn handle(&mut self, message: PauseState, _: &mut Context<Self>) -> Self::Result {
        let pause = if message.pause {
            polymer_pluginhost::controller::PauseState::Pause
        } else {
            polymer_pluginhost::controller::PauseState::Resume
        };
        let started = Instant::now();
        let response = self.state.with_active(message.token, |state| {
            state.game_host.pause_resume(pause, message.timeout)
        });
        let state = self.state.clone();
        Box::pin(async move {
            let result = match response {
                Ok(Some(response)) => response.await.map_err(|_| "emulator plugin session closed during pause/resume".to_owned()).and_then(|result| result),
                Ok(None) => Ok(()),
                Err(error) => Err(error),
            };
            tracing::info!(state = ?pause, elapsed_ms = started.elapsed().as_millis(), success = result.is_ok(), "polymer: pause/resume plugin callback completed");
            state.with_active(message.token, |state| {
                if let Err(error) = &result {
                    state.emit_instance_event(message.token, "pauseresumeerror", &instance_events::error(error));
                }
                result
            })
        }.into_actor(self))
    }
}

impl Handler<StateScreenshot> for GuiActor {
    type Result = Result<Option<Vec<u8>>, String>;
    fn handle(&mut self, message: StateScreenshot, _: &mut Context<Self>) -> Self::Result {
        self.state.with_active(message.token, |state| {
            state.game_host.save_state_screenshot(&message.record_id)
        })
    }
}
