use super::{ControllerActor, event_loop::DenoCall};
use crate::{controller::*, runtime::js::ExtensionFunction};
use actix::{ActorFutureExt, AtomicResponse, Context, Handler, fut};
use deno_core::{serde_v8, v8};

impl Handler<AttachAutomation> for ControllerActor {
    type Result = Result<(), String>;
    fn handle(&mut self, message: AttachAutomation, _: &mut Context<Self>) -> Self::Result {
        if self.control.is_cancelled() {
            return Err("controller plugin session is closed".to_owned());
        }
        crate::automation::attach(self.runtime.js_runtime_mut(), message.0);
        Ok(())
    }
}

impl Handler<AttachActions> for ControllerActor {
    type Result = Result<(), String>;
    fn handle(&mut self, message: AttachActions, _: &mut Context<Self>) -> Self::Result {
        if self.control.is_cancelled() {
            return Err("controller plugin session is closed".to_owned());
        }
        crate::actions::attach(self.runtime.js_runtime_mut(), message.0);
        Ok(())
    }
}

impl Handler<PauseResume> for ControllerActor {
    type Result = AtomicResponse<Self, Result<(), String>>;
    fn handle(&mut self, message: PauseResume, _: &mut Context<Self>) -> Self::Result {
        let call =
            ExtensionFunction::argument(self.runtime.js_runtime_mut(), message.state.as_str())
                .map_err(|e| e.to_string())
                .and_then(|argument| {
                    DenoCall::start(
                        self,
                        "pauseResume",
                        &[argument],
                        message.timeout,
                        "onPauseResume",
                    )
                });
        match call {
            Ok(call) => AtomicResponse::new(Box::pin(call.map(|result, _, _| result.map(|_| ())))),
            Err(error) => AtomicResponse::new(Box::pin(fut::ready(Err(error)))),
        }
    }
}

impl Handler<StateAction> for ControllerActor {
    type Result = AtomicResponse<Self, Result<(), String>>;
    fn handle(&mut self, message: StateAction, _: &mut Context<Self>) -> Self::Result {
        let call = (|| {
            let runtime = self.runtime.js_runtime_mut();
            let values = [
                ExtensionFunction::argument(runtime, message.slot)?,
                ExtensionFunction::argument(runtime, message.save)?,
            ];
            Ok::<_, deno_error::JsErrorBox>(values)
        })()
        .map_err(|e| e.to_string())
        .and_then(|values| {
            DenoCall::start(
                self,
                "stateAction",
                &values,
                message.timeout,
                if message.save {
                    "onStateSave"
                } else {
                    "onStateResume"
                },
            )
        });
        match call {
            Ok(call) => AtomicResponse::new(Box::pin(call.map(|result, _, _| result.map(|_| ())))),
            Err(error) => AtomicResponse::new(Box::pin(fut::ready(Err(error)))),
        }
    }
}

impl Handler<Quit> for ControllerActor {
    type Result = AtomicResponse<Self, Result<Option<Vec<u8>>, String>>;
    fn handle(&mut self, message: Quit, _: &mut Context<Self>) -> Self::Result {
        match DenoCall::start(self, "quit", &[], message.timeout, "onQuit") {
            Ok(call) => AtomicResponse::new(Box::pin(call.map(|result, actor, _| {
                let value = result?;
                deno_core::scope!(scope, actor.runtime.js_runtime_mut());
                let value = v8::Local::new(scope, value);
                serde_v8::from_v8::<Option<serde_bytes::ByteBuf>>(scope, value)
                    .map(|value| value.map(serde_bytes::ByteBuf::into_vec))
                    .map_err(|e| e.to_string())
            }))),
            Err(error) => AtomicResponse::new(Box::pin(fut::ready(Err(error)))),
        }
    }
}

impl Handler<ControllerChanged> for ControllerActor {
    type Result = AtomicResponse<Self, Result<polymer_controllers::ControllerTopology, String>>;
    fn handle(&mut self, message: ControllerChanged, _: &mut Context<Self>) -> Self::Result {
        let checkpoint = match crate::emulator::adapter::checkpoint_configuration(
            self.runtime.js_runtime_mut(),
            self.configuration_id,
        ) {
            Ok(checkpoint) => checkpoint,
            Err(error) => return AtomicResponse::new(Box::pin(fut::ready(Err(error.to_string())))),
        };
        let call = self.prepare_change(&message.0);
        match call {
            Ok(call) => AtomicResponse::new(Box::pin(call.map(move |result, actor, _| {
                let result = result.and_then(|value| {
                    deno_core::scope!(scope, actor.runtime.js_runtime_mut());
                    let value = v8::Local::new(scope, value);
                    serde_v8::from_v8(scope, value).map_err(|e| e.to_string())
                });
                actor.finish_change(result, checkpoint)
            }))),
            Err(error) => {
                let result = self.finish_change(Err(error), checkpoint);
                AtomicResponse::new(Box::pin(fut::ready(result)))
            }
        }
    }
}

impl ControllerActor {
    fn prepare_change(
        &mut self,
        change: &polymer_controllers::ControllerTopologyChange,
    ) -> Result<DenoCall, String> {
        let assignments = change
            .requested
            .port_assignments()
            .map_err(|e| e.to_string())?;
        let configuration = self
            .plugin
            .materialize(&self.console, assignments)
            .map_err(|e| e.to_string())?;
        let runtime = self.runtime.js_runtime_mut();
        crate::emulator::adapter::rematerialize_configuration(
            runtime,
            self.configuration_id,
            configuration,
            &change.requested,
        )
        .map_err(|e| e.to_string())?;
        let values = [
            ExtensionFunction::argument(runtime, &change.requested).map_err(|e| e.to_string())?,
            ExtensionFunction::argument(runtime, &change.previous).map_err(|e| e.to_string())?,
        ];
        DenoCall::start(
            self,
            "controllerChanged",
            &values,
            polymer_controllers::DEFAULT_CONTROLLER_TOPOLOGY_TRANSACTION_TIMEOUT,
            "onControllerChanged",
        )
    }

    fn finish_change(
        &mut self,
        result: Result<polymer_controllers::ControllerTopology, String>,
        checkpoint: crate::configuration::ConfigurationCheckpoint,
    ) -> Result<polymer_controllers::ControllerTopology, String> {
        let result = result.and_then(|topology| {
            crate::filesystem::flush_staged(self.runtime.js_runtime_mut())
                .map_err(|e| e.to_string())?;
            Ok(topology)
        });
        if let Err(error) = &result {
            crate::emulator::adapter::restore_configuration(
                self.runtime.js_runtime_mut(),
                self.configuration_id,
                checkpoint,
            );
            if !self.control.is_cancelled() {
                let restored = crate::emulator::adapter::refresh_configuration_mounts(
                    self.runtime.js_runtime_mut(),
                )
                .map_err(|e| e.to_string())
                .and_then(|()| {
                    crate::filesystem::flush_staged(self.runtime.js_runtime_mut())
                        .map_err(|e| e.to_string())
                });
                if let Err(error) = restored {
                    tracing::error!(
                        "polymer: restoring controller configuration files failed: {error}"
                    );
                }
            }
            crate::filesystem::discard_staged(self.runtime.js_runtime_mut());
            tracing::error!(plugin = %self.plugin.id(), "polymer: onControllerChanged failed: {error}");
        }
        result
    }
}
