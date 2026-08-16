//! Long-lived Deno runtime that drives the sole Blitz shell document.
//!
//! Plugin packages never enter this isolate. Pluginhost creates a separate
//! worker isolate for each native invocation.

pub(crate) mod deno;

use blitz_dom::BaseDocument;
use deno::EventResult;
use serde_json::Value;
use std::{collections::HashMap, error::Error};

pub(crate) use deno::{
    EmulatorCommand, InputCommand, InstanceToken, WebRtcCommand, WebRtcCommandSink,
};
pub(crate) use snowglass_deno::snowglass::{SnowglassNotification, SnowglassNotificationSender};

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
        input_commands: deno::InputCommandSink,
        webrtc_commands: WebRtcCommandSink,
        startup_request: Option<Value>,
        plugin_runtime: std::sync::Arc<snowglass_pluginhost::PluginRuntime>,
        viewer: Option<snowglass_renderer::cli::ViewerOptions>,
    ) -> Result<(Self, SnowglassNotificationSender), Box<dyn Error>> {
        let result = match deno::Runtime::serialized_snapshot(document, width, height) {
            Ok(initial_snapshot) => tokio::task::spawn_local(deno::Runtime::from_snapshot(
                initial_snapshot,
                html,
                source_name,
                emulator_commands,
                input_commands,
                webrtc_commands,
                startup_request,
                plugin_runtime,
                viewer,
            ))
            .await
            .unwrap_or_else(|error| Err(Box::new(error))),
            Err(error) => Err(Box::new(error) as Box<dyn Error>),
        };
        result.map(|(runtime, notifications)| {
            (
                Self {
                    runtime: Some(runtime),
                },
                notifications,
            )
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

pub(crate) use deno::{EmulatorCommandSink, InputCommandSink, Mutation};
