//! Long-lived Deno runtime that drives the sole Blitz shell document.
//!
//! Plugin packages never enter this isolate. Pluginhost creates a separate
//! worker isolate for each native invocation.

pub(crate) mod deno;
mod events;

use crate::shell::document::SharedShellDocument;
use blitz_dom::BaseDocument;
use deno::EventResult;
pub(crate) use events::CoalescedPointer;
use serde_json::Value;
use std::error::Error;

pub(crate) use polymer_deno::polymer::{
    InstanceToken, PolymerNotification, PolymerNotificationSender,
};

pub(crate) struct ShellRuntime {
    runtime: deno::Runtime,
}

pub(crate) struct ShellRuntimeOptions {
    pub debug: bool,
    pub html: String,
    pub source_name: String,
    pub commands: polymer_runtime::FrontendCommands,
    pub plugin_runtime: std::sync::Arc<polymer_pluginhost::runtime::PluginRuntime>,
    pub local_storage: Option<polymer_deno::webstorage::LocalStorage>,
    pub library: Option<polymer_runtime::LibraryService>,
    pub input_sources: polymer_input::InputHub,
    pub controller_api: polymer_runtime::ControllerApiState,
}

impl ShellRuntime {
    pub async fn start(
        document: SharedShellDocument,
        options: ShellRuntimeOptions,
    ) -> Result<(Self, PolymerNotificationSender), Box<dyn Error>> {
        let ShellRuntimeOptions {
            debug,
            html,
            source_name,
            commands,
            plugin_runtime,
            local_storage,
            library,
            input_sources,
            controller_api,
        } = options;
        let (runtime, notifications) = deno::Runtime::start(
            debug,
            document,
            html,
            source_name,
            commands,
            plugin_runtime,
            local_storage,
            library,
            input_sources,
            controller_api,
        )
        .await?;
        Ok((Self { runtime }, notifications))
    }

    pub fn resize(&mut self, width: u32, height: u32, scale_factor: f32) {
        if let Err(error) = self.runtime.resize(width, height, scale_factor) {
            tracing::error!("polymer: resizing the shell window failed: {error}");
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
                tracing::error!("polymer: dispatching shell {kind} failed: {error}");
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
        samples: &[CoalescedPointer],
    ) -> EventResult {
        let (event_class, mut init) = events::event_init(event);
        if matches!(event, blitz_traits::events::DomEventData::PointerMove(_))
            && !samples.is_empty()
        {
            events::attach_samples(&mut init, samples);
        }
        self.dispatch_dom(kind, target, event_class, &init, bubbles, cancelable)
    }

    pub fn poll_event_loop(&mut self, cx: &mut std::task::Context<'_>) {
        if let std::task::Poll::Ready(Err(error)) = self.runtime.poll_event_loop(cx) {
            tracing::error!("polymer: polling shell JavaScript failed: {error}");
        }
    }

    pub fn stage_layout(
        &mut self,
        document: &BaseDocument,
        width: u32,
        height: u32,
        scale_factor: f32,
    ) {
        self.runtime
            .stage_layout(document, width, height, scale_factor);
    }

    pub fn publish_layout(&mut self) {
        if let Err(error) = self.runtime.publish_layout() {
            tracing::error!("polymer: synchronizing the shell DOM failed: {error}");
        }
    }

    pub fn reload_requested(&self) -> bool {
        self.runtime.reload_requested()
    }

    pub(crate) fn inspector_address(&self) -> std::net::SocketAddr {
        self.runtime.inspector_address()
    }
}
