//! Long-lived Deno runtime that drives the background Blitz document.
//!
//! Plugin packages never enter this isolate. Its plugin facade is a client of
//! the shared bus; pluginhost creates a separate worker isolate for each call.

#[path = "background_runtime/deno/mod.rs"]
pub(crate) mod deno;
#[path = "background_runtime/launch_coordinator.rs"]
mod launch_coordinator;

use blitz_dom::BaseDocument;
use deno::EventResult;
use serde_json::Value;
use std::{collections::HashMap, error::Error};

pub(crate) use deno::{EmulatorCommand, InstanceToken};
pub(crate) use launch_coordinator::{BackgroundLaunchCoordinator, LaunchCoordinatorEvent};

pub(crate) struct BackgroundRuntime {
    runtime: Option<deno::Runtime>,
}

impl BackgroundRuntime {
    pub async fn start(
        document: &BaseDocument,
        width: u32,
        height: u32,
        html: String,
        source_name: String,
        emulator_commands: EmulatorCommandSink,
        background_bus: Option<crate::shared_bus::BackgroundBusEndpoint>,
    ) -> Self {
        let result = match deno::Runtime::serialized_snapshot(document, width, height) {
            Ok(initial_snapshot) => tokio::task::spawn_local(deno::Runtime::from_snapshot(
                initial_snapshot,
                html,
                source_name,
                emulator_commands,
                background_bus,
            ))
            .await
            .unwrap_or_else(|error| Err(Box::new(error))),
            Err(error) => Err(Box::new(error) as Box<dyn Error>),
        };
        match result {
            Ok(runtime) => Self {
                runtime: Some(runtime),
            },
            Err(error) => {
                tracing::error!("snowglass: starting background JavaScript failed: {error}");
                Self { runtime: None }
            }
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.resize(width, height)
        {
            tracing::error!("snowglass: resizing the background window failed: {error}");
        }
    }

    pub fn dispatch_mouse(&mut self, kind: &str, target: usize, event: MouseEvent) -> EventResult {
        let Some(runtime) = self.runtime.as_mut() else {
            return EventResult::default();
        };
        runtime
            .dispatch_mouse(kind, target, event)
            .unwrap_or_else(|error| {
                tracing::error!("snowglass: dispatching background {kind} failed: {error}");
                EventResult::default()
            })
    }

    pub fn dispatch_keyboard(&mut self, kind: &str, event: KeyboardEvent) -> EventResult {
        let Some(runtime) = self.runtime.as_mut() else {
            return EventResult::default();
        };
        runtime
            .dispatch_keyboard(kind, event)
            .unwrap_or_else(|error| {
                tracing::error!("snowglass: dispatching background {kind} failed: {error}");
                EventResult::default()
            })
    }

    pub fn post_message(&mut self, message: &Value) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.post_message(message)
        {
            tracing::error!("snowglass: delivering a background message failed: {error}");
        }
    }

    pub fn set_input_configuration(&mut self, configuration: &Value, layouts: &Value) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.set_input_configuration(configuration, layouts)
        {
            tracing::error!("snowglass: publishing background input configuration failed: {error}");
        }
    }

    pub fn input_configuration_result(&mut self, request_id: u64, result: &Value) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.input_configuration_result(request_id, result)
        {
            tracing::error!("snowglass: returning background input result failed: {error}");
        }
    }

    pub fn set_input_state(&mut self, controllers: &Value, priority: &str) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.set_input_state(controllers, priority)
        {
            tracing::error!("snowglass: publishing background input state failed: {error}");
        }
    }

    pub fn dispatch_emulator_event(&mut self, event: &Value) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.dispatch_emulator_event(event)
        {
            tracing::error!("snowglass: dispatching a background emulator event failed: {error}");
        }
    }

    pub async fn poll_event_loop(&mut self) -> bool {
        let Some(runtime) = self.runtime.as_mut() else {
            return false;
        };
        if let Err(error) = runtime.poll_event_loop().await {
            tracing::error!("snowglass: polling background JavaScript failed: {error}");
        }
        true
    }

    pub fn sync(
        &mut self,
        document: &BaseDocument,
        width: u32,
        height: u32,
        node_ids: &HashMap<usize, usize>,
    ) {
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.sync(document, width, height, node_ids)
        {
            tracing::error!("snowglass: synchronizing the background DOM failed: {error}");
        }
    }

    pub fn take_mutations(&mut self) -> Vec<Mutation> {
        self.runtime
            .as_mut()
            .map(deno::Runtime::take_mutations)
            .unwrap_or_default()
    }
}

pub(crate) use deno::{EmulatorCommandSink, KeyboardEvent, MouseEvent, Mutation};
