//! Long-lived Deno runtime that drives the sole Blitz shell document.
//!
//! Plugin packages never enter this isolate. Pluginhost creates a separate
//! worker isolate for each native invocation.

pub(crate) mod deno;
mod events;

use blitz_dom::BaseDocument;
use deno::EventResult;
use serde_json::Value;
use std::{collections::HashMap, error::Error};

pub(crate) use deno::{
    EmulatorCommand, InputCommand, InstanceToken, WebRtcCommand, WebRtcCommandSink,
};
pub(crate) use snowglass_deno::snowglass::{SnowglassNotification, SnowglassNotificationSender};

pub(crate) struct ShellRuntime {
    runtime: deno::Runtime,
}

pub(crate) struct ShellRuntimeOptions {
    pub html: String,
    pub source_name: String,
    pub emulator_commands: EmulatorCommandSink,
    pub input_commands: InputCommandSink,
    pub webrtc_commands: WebRtcCommandSink,
    pub plugin_runtime: std::sync::Arc<snowglass_pluginhost::PluginRuntime>,
    pub local_storage: Option<snowglass_deno::webstorage::LocalStorage>,
    pub input_sources: snowglass_input::InputHub,
    pub input_api: snowglass_runtime::InputApiState,
}

impl ShellRuntime {
    pub async fn start(
        document: &BaseDocument,
        width: u32,
        height: u32,
        scale_factor: f32,
        node_ids: &HashMap<blitz_dom::NodeId, usize>,
        options: ShellRuntimeOptions,
    ) -> Result<(Self, SnowglassNotificationSender), Box<dyn Error>> {
        let ShellRuntimeOptions {
            html,
            source_name,
            emulator_commands,
            input_commands,
            webrtc_commands,
            plugin_runtime,
            local_storage,
            input_sources,
            input_api,
        } = options;
        let result = match deno::Runtime::serialized_snapshot(
            document,
            width,
            height,
            scale_factor,
            node_ids,
        ) {
            Ok(initial_snapshot) => {
                deno::Runtime::from_snapshot(
                    initial_snapshot,
                    html,
                    source_name,
                    emulator_commands,
                    input_commands,
                    webrtc_commands,
                    plugin_runtime,
                    local_storage,
                    input_sources,
                    input_api,
                )
                .await
            }
            Err(error) => Err(Box::new(error) as Box<dyn Error>),
        };
        result.map(|(runtime, notifications)| (Self { runtime }, notifications))
    }

    pub fn resize(&mut self, width: u32, height: u32, scale_factor: f32) {
        if let Err(error) = self.runtime.resize(width, height, scale_factor) {
            tracing::error!("snowglass: resizing the shell window failed: {error}");
        }
    }

    fn dispatch_dom(
        &mut self,
        kind: &str,
        target: usize,
        event_class: &str,
        init: &Value,
        bubbles: bool,
        cancelable: bool,
    ) -> EventResult {
        self.runtime
            .dispatch_dom(kind, target, event_class, init, bubbles, cancelable)
            .unwrap_or_else(|error| {
                tracing::error!("snowglass: dispatching shell {kind} failed: {error}");
                EventResult::default()
            })
    }

    pub fn dispatch_event(
        &mut self,
        kind: &str,
        target: usize,
        event: &blitz_traits::events::DomEventData,
        bubbles: bool,
        cancelable: bool,
    ) -> EventResult {
        let (event_class, init) = events::event_init(event);
        self.dispatch_dom(kind, target, event_class, &init, bubbles, cancelable)
    }

    pub async fn poll_event_loop(&mut self) {
        if let Err(error) = self.runtime.poll_event_loop().await {
            tracing::error!("snowglass: polling shell JavaScript failed: {error}");
        }
    }

    pub fn sync(
        &mut self,
        document: &BaseDocument,
        width: u32,
        height: u32,
        scale_factor: f32,
        node_ids: &HashMap<blitz_dom::NodeId, usize>,
        roots: Option<&[blitz_dom::NodeId]>,
    ) {
        if let Err(error) =
            self.runtime
                .sync(document, width, height, scale_factor, node_ids, roots)
        {
            tracing::error!("snowglass: synchronizing the shell DOM failed: {error}");
        }
    }

    pub fn take_mutations(&mut self) -> Vec<Mutation> {
        self.runtime.take_mutations()
    }

    pub(crate) fn inspector_address(&self) -> std::net::SocketAddr {
        self.runtime.inspector_address()
    }
}

pub(crate) use deno::{EmulatorCommandSink, InputCommandSink, Mutation};
