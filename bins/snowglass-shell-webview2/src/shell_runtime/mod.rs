//! Long-lived Deno runtime that drives the sole Blitz shell document.
//!
//! Plugin packages never enter this isolate. Pluginhost creates a separate
//! worker isolate for each native invocation.

pub(crate) mod deno;
mod launch_coordinator;

use blitz_dom::BaseDocument;
use deno::EventResult;
use serde_json::Value;
use std::{collections::HashMap, error::Error};

pub(crate) use deno::{EmulatorCommand, InstanceToken};
pub(crate) use deno::{LauncherCommand, LauncherCommandSink};
pub(crate) use deno::{WebRtcCommand, WebRtcCommandSink};
pub(crate) use launch_coordinator::{LaunchCoordinatorEvent, ShellLaunchCoordinator};

pub(crate) struct ShellRuntime {
    runtime: Option<deno::Runtime>,
}

impl ShellRuntime {
    pub async fn start(
        document: &BaseDocument,
        width: u32,
        height: u32,
        html: String,
        source_name: String,
        emulator_commands: EmulatorCommandSink,
        webrtc_commands: WebRtcCommandSink,
        launcher_commands: LauncherCommandSink,
        initial_launcher_state: Value,
        plugin_runtime: std::sync::Arc<snowglass_pluginhost::PluginRuntime>,
    ) -> Result<Self, Box<dyn Error>> {
        let result = match deno::Runtime::serialized_snapshot(document, width, height) {
            Ok(initial_snapshot) => tokio::task::spawn_local(deno::Runtime::from_snapshot(
                initial_snapshot,
                html,
                source_name,
                emulator_commands,
                webrtc_commands,
                launcher_commands,
                initial_launcher_state,
                plugin_runtime,
            ))
            .await
            .unwrap_or_else(|error| Err(Box::new(error))),
            Err(error) => Err(Box::new(error) as Box<dyn Error>),
        };
        result.map(|runtime| Self {
            runtime: Some(runtime),
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.resize(width, height)
        {
            tracing::error!("snowglass: resizing the shell window failed: {error}");
        }
    }

    pub fn dispatch_dom(
        &mut self,
        kind: &str,
        target: usize,
        event_class: &str,
        init: &Value,
        bubbles: bool,
        cancelable: bool,
    ) -> EventResult {
        let Some(runtime) = self.runtime.as_mut() else {
            return EventResult::default();
        };
        runtime
            .dispatch_dom(kind, target, event_class, init, bubbles, cancelable)
            .unwrap_or_else(|error| {
                tracing::error!("snowglass: dispatching shell {kind} failed: {error}");
                EventResult::default()
            })
    }

    pub fn set_launcher_state(&mut self, state: &Value) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.set_launcher_state(state)
        {
            tracing::error!("snowglass: publishing launcher state failed: {error}");
        }
    }

    pub fn set_launch_plan(&mut self, plan: Option<&Value>) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.set_launch_plan(plan)
        {
            tracing::error!("snowglass: publishing launch plan failed: {error}");
        }
    }

    pub fn set_drop_active(&mut self, active: bool) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.set_drop_active(active)
        {
            tracing::error!("snowglass: publishing file-hover state failed: {error}");
        }
    }

    pub fn set_input_configuration(&mut self, configuration: &Value, layouts: &Value) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.set_input_configuration(configuration, layouts)
        {
            tracing::error!("snowglass: publishing shell input configuration failed: {error}");
        }
    }

    pub fn input_configuration_result(&mut self, request_id: u64, result: &Value) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.input_configuration_result(request_id, result)
        {
            tracing::error!("snowglass: returning shell input result failed: {error}");
        }
    }

    pub fn set_input_state(&mut self, controllers: &Value, priority: &str) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.set_input_state(controllers, priority)
        {
            tracing::error!("snowglass: publishing shell input state failed: {error}");
        }
    }

    pub fn dispatch_emulator_event(&mut self, event: &Value) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.dispatch_emulator_event(event)
        {
            tracing::error!("snowglass: dispatching a shell emulator event failed: {error}");
        }
    }

    pub async fn poll_event_loop(&mut self) -> bool {
        let Some(runtime) = self.runtime.as_mut() else {
            return false;
        };
        if let Err(error) = runtime.poll_event_loop().await {
            tracing::error!("snowglass: polling shell JavaScript failed: {error}");
        }
        true
    }

    pub fn sync(
        &mut self,
        document: &BaseDocument,
        width: u32,
        height: u32,
        node_ids: &HashMap<usize, usize>,
        roots: Option<&[usize]>,
    ) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.sync(document, width, height, node_ids, roots)
        {
            tracing::error!("snowglass: synchronizing the shell DOM failed: {error}");
        }
    }

    pub fn take_mutations(&mut self) -> Vec<Mutation> {
        self.runtime
            .as_mut()
            .map(deno::Runtime::take_mutations)
            .unwrap_or_default()
    }
}

pub(crate) use deno::{EmulatorCommandSink, Mutation};
