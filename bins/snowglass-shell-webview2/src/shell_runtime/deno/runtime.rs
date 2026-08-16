use std::collections::HashMap;
use std::rc::Rc;
#[cfg(not(test))]
use std::sync::OnceLock;

use blitz_dom::BaseDocument;
use deno_core::ModuleSpecifier;
use serde_json::Value;

use super::dom::{
    Mutation, layout_snapshot, op_dom_append_child, op_dom_create_comment, op_dom_create_element,
    op_dom_create_text, op_dom_focus, op_dom_insert_before, op_dom_parse_fragment,
    op_dom_remove_attribute, op_dom_remove_child, op_dom_set_attribute, op_dom_set_text,
    op_dom_take_layout, snapshot,
};
use super::emulator::{
    EmulatorBridge, EmulatorCommandSink, op_emulator_close, op_emulator_input, op_emulator_launch,
    op_emulator_message, op_emulator_set_muted, op_emulator_set_volume, op_input_configure,
};
use super::input::{EventResult, op_event_result};
use super::launcher::{
    LauncherBridge, LauncherCommandSink, op_launcher_cancel, op_launcher_open, op_launcher_settle,
    op_launcher_start,
};
use super::modules::{BrowserModuleLoader, document_scripts, execute_document_script};
use super::state::{BridgeState, op_post_message};

const BOOTSTRAP: &str = include_str!("runtime.js");
const EVENT_DISPATCH: &str = include_str!("event_dispatch.js");
const INPUT_FACADE: &str = include_str!("input_facade.js");
const EMULATOR_FACADE: &str = include_str!("emulator_facade.js");
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
        op_emulator_launch,
        op_emulator_close,
        op_emulator_set_volume,
        op_emulator_set_muted,
        op_emulator_message,
        op_emulator_input,
        op_input_configure,
        op_launcher_open,
        op_launcher_start,
        op_launcher_cancel,
        op_launcher_settle,
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
                  op_emulator_close,
                  op_emulator_input,
                  op_input_configure,
                  op_launcher_open,
                  op_launcher_start,
                  op_launcher_cancel,
                  op_launcher_settle,
                  op_emulator_launch,
                  op_emulator_message,
                  op_emulator_set_muted,
                  op_emulator_set_volume,
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
                      op_emulator_close,
                      op_emulator_input,
                      op_input_configure,
                      op_launcher_open,
                      op_launcher_start,
                      op_launcher_cancel,
                      op_launcher_settle,
                      op_emulator_launch,
                      op_emulator_message,
                      op_emulator_set_muted,
                      op_emulator_set_volume,
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
        state.put(EmulatorBridge::default());
        state.put(LauncherBridge::default());
    },
);

pub struct Runtime {
    worker: snowglass_deno::Runtime,
    layout_cache: HashMap<usize, [f32; 4]>,
    layout_view_state: Option<(Option<usize>, u32, u32)>,
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
        Self::from_snapshot(
            Self::serialized_snapshot(document, width, height)?,
            html.to_owned(),
            source_name.to_owned(),
            emulator_commands,
            LauncherCommandSink::default(),
            serde_json::json!({ "kind": "idle" }),
        )
        .await
    }

    pub(in crate::shell_runtime) fn serialized_snapshot(
        document: &BaseDocument,
        width: u32,
        height: u32,
    ) -> Result<String, serde_json::Error> {
        serde_json::to_string(&snapshot(document, width, height, None))
    }

    pub(in crate::shell_runtime) async fn from_snapshot(
        initial_snapshot: String,
        html: String,
        source_name: String,
        emulator_commands: EmulatorCommandSink,
        launcher_commands: LauncherCommandSink,
        initial_launcher_state: Value,
    ) -> Result<Self, Box<dyn std::error::Error>> {
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
        let mut worker = snowglass_deno::Runtime::new(snowglass_deno::RuntimeOptions {
            main_module: document_url.clone(),
            module_loader: browser_loader.clone(),
            extensions: vec![snowglass_shell_runtime::init()],
            user_agent: format!("Snowglass/{}", env!("CARGO_PKG_VERSION")),
            storage_dir,
            inspect: true,
        });
        worker
            .js_runtime_mut()
            .execute_script("snowglass:shell/event-dispatch.js", EVENT_DISPATCH)?;
        worker
            .js_runtime_mut()
            .op_state()
            .borrow_mut()
            .borrow_mut::<EmulatorBridge>()
            .command_sink = emulator_commands;
        worker
            .js_runtime_mut()
            .op_state()
            .borrow_mut()
            .borrow_mut::<LauncherBridge>()
            .0 = launcher_commands;
        worker
            .js_runtime_mut()
            .execute_script("snowglass:shell/input-facade.js", INPUT_FACADE)?;
        worker
            .js_runtime_mut()
            .execute_script("snowglass:shell/emulator-facade.js", EMULATOR_FACADE)?;
        worker
            .js_runtime_mut()
            .execute_script("snowglass:shell/runtime.js", BOOTSTRAP)?;
        worker.js_runtime_mut().execute_script(
            "snowglass:shell/document.js",
            format!("__snowglassDom.install({initial_snapshot});"),
        )?;
        worker.js_runtime_mut().execute_script(
            "snowglass:shell/initial-launcher-state.js",
            format!(
                "__snowglassDom.setLauncherState({});",
                serde_json::to_string(&initial_launcher_state)?
            ),
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
        Ok(Self {
            worker,
            layout_cache: HashMap::new(),
            layout_view_state: None,
        })
    }

    pub fn sync(
        &mut self,
        document: &BaseDocument,
        width: u32,
        height: u32,
        node_ids: &HashMap<usize, usize>,
        roots: Option<&[usize]>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let layout = layout_snapshot(
            document,
            width,
            height,
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

    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), Box<dyn std::error::Error>> {
        self.worker.js_runtime_mut().execute_script(
            "snowglass:shell/resize.js",
            format!("__snowglassDom.resize({width}, {height});"),
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

    pub fn set_launcher_state(&mut self, state: &Value) -> Result<(), Box<dyn std::error::Error>> {
        self.worker.js_runtime_mut().execute_script(
            "snowglass:shell/launcher-state.js",
            format!(
                "__snowglassDom.setLauncherState({});",
                serde_json::to_string(state)?
            ),
        )?;
        Ok(())
    }

    pub fn set_launch_plan(
        &mut self,
        plan: Option<&Value>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let plan = plan
            .map(serde_json::to_string)
            .transpose()?
            .unwrap_or_else(|| "null".into());
        self.worker.js_runtime_mut().execute_script(
            "snowglass:shell/launch-plan.js",
            format!("__snowglassDom.setLaunchPlan({plan});"),
        )?;
        Ok(())
    }

    pub fn set_drop_active(&mut self, active: bool) -> Result<(), Box<dyn std::error::Error>> {
        self.worker.js_runtime_mut().execute_script(
            "snowglass:shell/drop-active.js",
            format!("__snowglassDom.setDropActive({active});"),
        )?;
        Ok(())
    }

    pub fn dispatch_emulator_event(
        &mut self,
        event: &Value,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.worker.js_runtime_mut().execute_script(
            "snowglass:shell/emulator-event.js",
            format!(
                "__snowglassDom.dispatchEmulatorEvent({});",
                serde_json::to_string(event)?
            ),
        )?;
        Ok(())
    }

    pub fn set_input_configuration(
        &mut self,
        configuration: &Value,
        layouts: &Value,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.worker.js_runtime_mut().execute_script(
            "snowglass:shell/input-configuration.js",
            format!(
                "__snowglassDom.setInputConfiguration({}, {});",
                serde_json::to_string(configuration)?,
                serde_json::to_string(layouts)?
            ),
        )?;
        Ok(())
    }

    pub fn set_input_state(
        &mut self,
        controllers: &Value,
        priority: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.worker.js_runtime_mut().execute_script(
            "snowglass:shell/input-state.js",
            format!(
                "__snowglassDom.setInputState({}, {});",
                serde_json::to_string(controllers)?,
                serde_json::to_string(priority)?
            ),
        )?;
        Ok(())
    }

    pub fn input_configuration_result(
        &mut self,
        request_id: u64,
        result: &Value,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.worker.js_runtime_mut().execute_script(
            "snowglass:shell/input-configuration-result.js",
            format!(
                "__snowglassDom.inputConfigurationResult({request_id}, {});",
                serde_json::to_string(result)?
            ),
        )?;
        Ok(())
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
