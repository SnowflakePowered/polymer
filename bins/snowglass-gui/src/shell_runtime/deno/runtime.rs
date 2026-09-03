use std::collections::HashMap;
use std::rc::Rc;

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
use snowglass_deno::snowglass::SnowglassNotificationSender;
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
                    runMicrotasks: core.runMicrotasks,
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

struct DenoRealm {
    worker: Option<snowglass_deno::Runtime>,
    executor: Option<tokio::runtime::Runtime>,
}

impl DenoRealm {
    fn new(worker: snowglass_deno::Runtime, executor: tokio::runtime::Runtime) -> Self {
        Self {
            worker: Some(worker),
            executor: Some(executor),
        }
    }

    fn parts(&mut self) -> (&mut snowglass_deno::Runtime, &tokio::runtime::Runtime) {
        (
            self.worker.as_mut().expect("Deno worker is initialized"),
            self.executor
                .as_ref()
                .expect("Deno executor is initialized"),
        )
    }

    async fn poll_event_loop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let (worker, executor) = self.parts();
        tokio::task::block_in_place(|| {
            executor.block_on(worker.run_up_to_duration(std::time::Duration::ZERO))
        })?;
        Ok(())
    }
}

impl Drop for DenoRealm {
    fn drop(&mut self) {
        let Some(executor) = self.executor.take() else {
            return;
        };
        if let Some(worker) = self.worker.take() {
            let _executor = executor.enter();
            drop(worker);
        }
        executor.shutdown_background();
    }
}

pub struct Runtime {
    realm: DenoRealm,
    inspector_address: std::net::SocketAddr,
    layout_cache: HashMap<usize, [f32; 4]>,
    layout_view_state: Option<(Option<usize>, u32, u32, u32)>,
}

impl Runtime {
    pub(in crate::shell_runtime) fn serialized_snapshot(
        document: &BaseDocument,
        width: u32,
        height: u32,
        device_pixel_ratio: f32,
        node_ids: &HashMap<blitz_dom::NodeId, usize>,
    ) -> Result<String, serde_json::Error> {
        serde_json::to_string(&snapshot(
            document,
            width,
            height,
            device_pixel_ratio,
            Some(node_ids),
        ))
    }

    pub(in crate::shell_runtime) async fn from_snapshot(
        initial_snapshot: String,
        html: String,
        source_name: String,
        emulator_commands: EmulatorCommandSink,
        input_commands: InputCommandSink,
        webrtc_commands: WebRtcCommandSink,
        plugin_runtime: std::sync::Arc<snowglass_pluginhost::PluginRuntime>,
        local_storage: Option<snowglass_deno::webstorage::LocalStorage>,
        input_sources: snowglass_input::InputHub,
        controller_api: snowglass_runtime::ControllerApiState,
    ) -> Result<(Self, SnowglassNotificationSender), Box<dyn std::error::Error>> {
        // This process also links WebRTC's ring-backed rustls feature, while
        // the selected Deno fetch/websocket extensions use aws-lc-rs. With both providers compiled rustls
        // requires the embedder to choose one before fetch creates a client.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let inspector_address = Self::start_inspector_server()?;
        let executor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let worker_result = tokio::task::block_in_place(|| {
            executor.block_on(async move {
                let document_url = ModuleSpecifier::parse(&source_name)?;
                let browser_loader = Rc::new(BrowserModuleLoader::new());
                let (snowglass_extension, notifications) =
                    snowglass_deno::snowglass::init_with_notifications(|notifications| {
                        snowglass_runtime::FrontendServices::new(
                            plugin_runtime.clone(),
                            Some(snowglass_runtime::ConfigurationDefaults::default()),
                            input_sources.clone(),
                            controller_api.clone(),
                            emulator_commands,
                            input_commands,
                            webrtc_commands,
 n                            notifications,
                        )
                    });
                let mut worker = snowglass_deno::Runtime::new(snowglass_deno::RuntimeOptions {
                    main_module: document_url.clone(),
                    module_loader: browser_loader.clone(),
                    extensions: vec![snowglass_extension, snowglass_shell_runtime::init()],
                    user_agent: format!("Snowglass/{}", env!("CARGO_PKG_VERSION")),
                    local_storage,
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
                worker.run_up_to_duration(std::time::Duration::ZERO).await?;
                Ok::<_, Box<dyn std::error::Error>>((worker, notifications))
            })
        });
        let (worker, notifications) = match worker_result {
            Ok(result) => result,
            Err(error) => {
                executor.shutdown_background();
                return Err(error);
            }
        };
        Ok((
            Self {
                realm: DenoRealm::new(worker, executor),
                inspector_address,
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
        node_ids: &HashMap<blitz_dom::NodeId, usize>,
        roots: Option<&[blitz_dom::NodeId]>,
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
        let (worker, executor) = self.realm.parts();
        let _executor = executor.enter();
        worker
            .js_runtime_mut()
            .op_state()
            .borrow_mut()
            .borrow_mut::<BridgeState>()
            .layout = Some(layout);
        worker
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
        let (worker, executor) = self.realm.parts();
        let _executor = executor.enter();
        worker.js_runtime_mut().execute_script(
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
        let (worker, executor) = self.realm.parts();
        let _executor = executor.enter();
        worker
            .js_runtime_mut()
            .op_state()
            .borrow_mut()
            .borrow_mut::<BridgeState>()
            .event = EventResult::default();
        worker.js_runtime_mut().execute_script(
            "snowglass:shell/dom-event.js",
            format!(
                "__snowglassDom.dispatchDom({}, {target}, {}, {}, {bubbles}, {cancelable});",
                serde_json::to_string(kind)?,
                serde_json::to_string(event_class)?,
                serde_json::to_string(init)?,
            ),
        )?;
        Ok(worker
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
        self.realm.poll_event_loop().await
    }

    pub fn take_mutations(&mut self) -> Vec<Mutation> {
        let (worker, executor) = self.realm.parts();
        let _executor = executor.enter();
        std::mem::take(
            &mut worker
                .js_runtime_mut()
                .op_state()
                .borrow_mut()
                .borrow_mut::<BridgeState>()
                .mutations,
        )
    }

    pub(crate) const fn inspector_address(&self) -> std::net::SocketAddr {
        self.inspector_address
    }

    fn start_inspector_server() -> Result<std::net::SocketAddr, Box<dyn std::error::Error>> {
        let server = snowglass_deno::create_inspector_server(
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            "snowglass",
            snowglass_deno::InspectPublishUid {
                console: false,
                http: true,
            },
        )?;
        tracing::info!(
            "snowglass: shell DevTools targets: http://{}/json",
            server.host
        );
        Ok(server.host)
    }
}
