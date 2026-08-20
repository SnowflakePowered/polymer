use std::rc::Rc;
#[cfg(not(test))]
use std::sync::OnceLock;
use std::{collections::HashMap, sync::Arc};

use blitz_dom::BaseDocument;
use deno_core::ModuleSpecifier;
use serde_json::Value;

use super::dom::{
    Mutation, layout_snapshot, op_dom_append_child, op_dom_create_comment, op_dom_create_element,
    op_dom_create_text, op_dom_focus, op_dom_insert_before, op_dom_parse_fragment,
    op_dom_remove_attribute, op_dom_remove_child, op_dom_set_attribute, op_dom_set_text,
    op_dom_take_layout, snapshot,
};
use super::input::{EventResult, op_event_result};
use super::modules::{BrowserModuleLoader, document_scripts, execute_document_script};
use super::state::{BridgeState, op_post_message};
use snowglass_deno::snowglass::{SnowglassNotification, SnowglassNotificationSender};
use snowglass_runtime::{EmulatorCommandSink, InputCommandSink, WebRtcCommandSink};

const BOOTSTRAP: &str = include_str!("runtime.js");
const EVENT_DISPATCH: &str = include_str!("event_dispatch.js");

deno_core::extension!(
    snowglass_shell_runtime,
    ops = [
        op_dom_create_element,
        op_dom_create_text,
        op_dom_create_comment,
        op_dom_append_child,
        op_dom_insert_before,
        op_dom_parse_fragment,
        op_dom_remove_child,
        op_dom_set_text,
        op_dom_set_attribute,
        op_dom_remove_attribute,
        op_dom_focus,
        op_dom_take_layout,
        op_post_message,
        op_event_result,
    ],
    esm_entry_point = "ext:snowglass_shell_runtime/bootstrap.js",
    esm = ["ext:snowglass_shell_runtime/bootstrap.js" = {
        source = r#"
                import { core } from "ext:core/mod.js";
                import {
                  op_dom_append_child,
                  op_dom_create_comment,
                  op_dom_create_element,
                  op_dom_create_text,
                  op_dom_insert_before,
                  op_dom_parse_fragment,
                  op_dom_remove_attribute,
                  op_dom_focus,
                  op_dom_take_layout,
                  op_dom_remove_child,
                  op_dom_set_attribute,
                  op_dom_set_text,
                  op_event_result,
                  op_post_message,
                } from "ext:core/ops";
                const webEvents = core.loadExtScript("ext:deno_web/02_event.js");
                Object.defineProperty(globalThis, "__snowglassRuntime", {
                  value: Object.freeze({
                    ops: Object.freeze({
                      op_dom_append_child,
                      op_dom_create_comment,
                      op_dom_create_element,
                      op_dom_create_text,
                      op_dom_insert_before,
                      op_dom_parse_fragment,
                      op_dom_remove_attribute,
                      op_dom_focus,
                      op_dom_take_layout,
                      op_dom_remove_child,
                      op_dom_set_attribute,
                      op_dom_set_text,
                      op_event_result,
                      op_post_message,
                    }),
                    webEvents,
                  }),
                  configurable: true,
                });
            "#
    },],
    state = |state| {
        state.put(BridgeState::default());
    },
);

pub struct Runtime {
    worker: snowglass_deno::Runtime,
    layout_cache: HashMap<usize, [f32; 4]>,
    layout_view_state: Option<(Option<usize>, u32, u32, u32)>,
}

impl Runtime {
    #[cfg(test)]
    pub async fn new(
        document: &BaseDocument,
        html: &str,
        source_name: &str,
        width: u32,
        height: u32,
        emulator_commands: EmulatorCommandSink,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let plugin_runtime = std::sync::Arc::new(snowglass_pluginhost::PluginRuntime::new(
            snowglass_pluginhost::PluginRegistry::new(),
            snowglass_pluginhost::PluginRuntimeOptions::default(),
        ));
        Self::new_with_plugin_runtime(
            document,
            html,
            source_name,
            width,
            height,
            emulator_commands,
            plugin_runtime,
        )
        .await
    }

    #[cfg(test)]
    pub async fn new_with_plugin_runtime(
        document: &BaseDocument,
        html: &str,
        source_name: &str,
        width: u32,
        height: u32,
        emulator_commands: EmulatorCommandSink,
        plugin_runtime: std::sync::Arc<snowglass_pluginhost::PluginRuntime>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::new_with_command_sinks(
            document,
            html,
            source_name,
            width,
            height,
            emulator_commands,
            InputCommandSink::default(),
            WebRtcCommandSink::default(),
            plugin_runtime,
        )
        .await
    }

    #[cfg(test)]
    pub async fn new_with_command_sinks(
        document: &BaseDocument,
        html: &str,
        source_name: &str,
        width: u32,
        height: u32,
        emulator_commands: EmulatorCommandSink,
        input_commands: InputCommandSink,
        webrtc_commands: WebRtcCommandSink,
        plugin_runtime: std::sync::Arc<snowglass_pluginhost::PluginRuntime>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (runtime, _notifications) = Self::new_with_command_sinks_and_notifications(
            document,
            html,
            source_name,
            width,
            height,
            emulator_commands,
            input_commands,
            webrtc_commands,
            plugin_runtime,
        )
        .await?;
        Ok(runtime)
    }

    #[cfg(test)]
    pub async fn new_with_command_sinks_and_notifications(
        document: &BaseDocument,
        html: &str,
        source_name: &str,
        width: u32,
        height: u32,
        emulator_commands: EmulatorCommandSink,
        input_commands: InputCommandSink,
        webrtc_commands: WebRtcCommandSink,
        plugin_runtime: std::sync::Arc<snowglass_pluginhost::PluginRuntime>,
    ) -> Result<(Self, SnowglassNotificationSender), Box<dyn std::error::Error>> {
        Self::from_snapshot(
            Self::serialized_snapshot(document, width, height, 1.0)?,
            html.to_owned(),
            source_name.to_owned(),
            emulator_commands,
            input_commands,
            webrtc_commands,
            None,
            plugin_runtime,
            None,
        )
        .await
    }

    pub(in crate::shell_runtime) fn serialized_snapshot(
        document: &BaseDocument,
        width: u32,
        height: u32,
        device_pixel_ratio: f32,
    ) -> Result<String, serde_json::Error> {
        serde_json::to_string(&snapshot(document, width, height, device_pixel_ratio, None))
    }

    pub(in crate::shell_runtime) async fn from_snapshot(
        initial_snapshot: String,
        html: String,
        source_name: String,
        emulator_commands: EmulatorCommandSink,
        input_commands: InputCommandSink,
        webrtc_commands: WebRtcCommandSink,
        startup_request: Option<Value>,
        plugin_runtime: std::sync::Arc<snowglass_pluginhost::PluginRuntime>,
        viewer: Option<snowglass_renderer::cli::ViewerOptions>,
    ) -> Result<(Self, SnowglassNotificationSender), Box<dyn std::error::Error>> {
        // This process also links WebRTC's ring-backed rustls feature, while
        // the selected Deno fetch/websocket extensions use aws-lc-rs. With both providers compiled rustls
        // requires the embedder to choose one before fetch creates a client.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let document_url = ModuleSpecifier::parse(&source_name)?;
        start_inspector_server()?;
        let browser_loader = Rc::new(BrowserModuleLoader::new());
        let storage_dir = std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .map(|path| path.join("Snowglass").join("webstorage"));
        let platform = Arc::new(snowglass_runtime::CommandRuntimePlatform::new(
            emulator_commands,
            input_commands,
            webrtc_commands,
        ));
        let services = snowglass_runtime::FrontendServices::new(
            plugin_runtime.clone(),
            viewer.as_ref().map(configuration_defaults),
            platform,
        );
        let (snowglass_extension, notifications) = snowglass_deno::snowglass::init(services);
        let mut worker = snowglass_deno::Runtime::new(snowglass_deno::RuntimeOptions {
            main_module: document_url.clone(),
            module_loader: browser_loader.clone(),
            extensions: vec![snowglass_extension, snowglass_shell_runtime::init()],
            user_agent: format!("Snowglass/{}", env!("CARGO_PKG_VERSION")),
            storage_dir,
            inspect: true,
        });
        worker
            .js_runtime_mut()
            .execute_script("snowglass:shell/event-dispatch.js", EVENT_DISPATCH)?;
        worker
            .js_runtime_mut()
            .execute_script("snowglass:shell/runtime.js", BOOTSTRAP)?;
        worker.js_runtime_mut().execute_script(
            "snowglass:shell/document.js",
            format!("__snowglassDom.install({initial_snapshot});"),
        )?;
        for (index, script) in document_scripts(&html).enumerate() {
            let result = execute_document_script(
                &mut worker,
                browser_loader.as_ref(),
                &document_url,
                index,
                script,
            )
            .await;
            if let Err(error) = result {
                // A browser reports an individual script error without
                // discarding the document's realm or preventing later scripts.
                tracing::error!("snowglass: shell script {index} failed: {error}");
            }
        }
        if let Some(request) = startup_request {
            notifications.send(SnowglassNotification::OpenRequested(request))?;
        }
        worker.run_up_to_duration(std::time::Duration::ZERO).await?;
        Ok((
            Self {
                worker,
                layout_cache: HashMap::new(),
                layout_view_state: None,
            },
            notifications,
        ))
    }

    pub fn sync(
        &mut self,
        document: &BaseDocument,
        width: u32,
        height: u32,
        device_pixel_ratio: f32,
        node_ids: &HashMap<usize, usize>,
        roots: Option<&[usize]>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let layout = layout_snapshot(
            document,
            width,
            height,
            device_pixel_ratio,
            node_ids,
            &mut self.layout_cache,
            roots,
        );
        let view_state = layout.view_state();
        if !layout.has_geometry_changes() && self.layout_view_state == Some(view_state) {
            return Ok(());
        }
        self.layout_view_state = Some(view_state);
        self.worker
            .js_runtime_mut()
            .op_state()
            .borrow_mut()
            .borrow_mut::<BridgeState>()
            .layout = Some(layout);
        self.worker
            .js_runtime_mut()
            .execute_script("snowglass:shell/sync.js", "__snowglassDom.syncLayout();")?;
        Ok(())
    }

    pub fn resize(
        &mut self,
        width: u32,
        height: u32,
        device_pixel_ratio: f32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.worker.js_runtime_mut().execute_script(
            "snowglass:shell/resize.js",
            format!("__snowglassDom.resize({width}, {height}, {device_pixel_ratio});"),
        )?;
        Ok(())
    }

    pub fn dispatch_dom(
        &mut self,
        kind: &str,
        target: usize,
        event_class: &str,
        init: &Value,
        bubbles: bool,
        cancelable: bool,
    ) -> Result<EventResult, Box<dyn std::error::Error>> {
        self.worker
            .js_runtime_mut()
            .op_state()
            .borrow_mut()
            .borrow_mut::<BridgeState>()
            .event = EventResult::default();
        self.worker.js_runtime_mut().execute_script(
            "snowglass:shell/dom-event.js",
            format!(
                "__snowglassDom.dispatchDom({}, {target}, {}, {}, {bubbles}, {cancelable});",
                serde_json::to_string(kind)?,
                serde_json::to_string(event_class)?,
                serde_json::to_string(init)?,
            ),
        )?;
        Ok(self
            .worker
            .js_runtime()
            .op_state()
            .borrow()
            .borrow::<BridgeState>()
            .event)
    }

    /// Polls Deno once without waiting for the realm to become idle.
    ///
    /// A browser realm may keep timers, sockets, or an inspector session alive
    /// indefinitely. The shell's event loop calls this regularly so those
    /// tasks progress without blocking the window thread.
    pub async fn poll_event_loop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.worker
            .run_up_to_duration(std::time::Duration::ZERO)
            .await?;
        Ok(())
    }

    pub fn take_mutations(&mut self) -> Vec<Mutation> {
        std::mem::take(
            &mut self
                .worker
                .js_runtime_mut()
                .op_state()
                .borrow_mut()
                .borrow_mut::<BridgeState>()
                .mutations,
        )
    }

    #[cfg(test)]
    pub fn take_messages(&mut self) -> Vec<Value> {
        std::mem::take(
            &mut self
                .worker
                .js_runtime_mut()
                .op_state()
                .borrow_mut()
                .borrow_mut::<BridgeState>()
                .messages,
        )
    }
}

fn configuration_defaults(
    viewer: &snowglass_renderer::cli::ViewerOptions,
) -> snowglass_runtime::ConfigurationDefaults {
    snowglass_runtime::ConfigurationDefaults {
        gamepads: viewer
            .gamepads
            .iter()
            .map(|gamepad| snowglass_runtime::DefaultGamepad {
                selector: gamepad.selector.clone(),
                slot: gamepad.slot,
                layout: gamepad.layout.clone(),
                force_mouse: gamepad.force_mouse,
            })
            .collect(),
        machine: viewer.machine.clone(),
        webrtc_targets: viewer.webrtc.clone(),
        webrtc_bind: viewer.bind_webrtc,
    }
}

fn start_inspector_server() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(test)]
    return Ok(());

    #[cfg(not(test))]
    static INSPECTOR: OnceLock<Result<std::net::SocketAddr, String>> = OnceLock::new();
    #[cfg(not(test))]
    match INSPECTOR.get_or_init(|| {
        let server = snowglass_deno::create_inspector_server(
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            "snowglass",
            snowglass_deno::InspectPublishUid {
                console: false,
                http: true,
            },
        )
        .map_err(|error| error.to_string())?;
        tracing::info!(
            "snowglass: shell DevTools targets: http://{}/json",
            server.host
        );
        Ok(server.host)
    }) {
        Ok(_) => Ok(()),
        Err(error) => Err(std::io::Error::other(error.clone()).into()),
    }
}
