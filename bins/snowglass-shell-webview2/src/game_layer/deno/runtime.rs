use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
#[cfg(not(test))]
use std::sync::OnceLock;

use blitz_dom::BaseDocument;
use deno_core::ModuleSpecifier;
use deno_runtime::deno_permissions::{PermissionsContainer, RuntimePermissionDescriptorParser};
use deno_runtime::worker::{MainWorker, WorkerOptions, WorkerServiceOptions};
use deno_runtime::{BootstrapOptions, FeatureChecker};
use serde_json::Value;

use super::dom::{
    Mutation, op_dom_append_child, op_dom_create_comment, op_dom_create_element,
    op_dom_create_text, op_dom_insert_before, op_dom_remove_attribute, op_dom_remove_child,
    op_dom_set_attribute, op_dom_set_text, snapshot,
};
use super::emulator::{
    EmulatorBridge, EmulatorCommandSink, op_emulator_close, op_emulator_input, op_emulator_launch,
    op_emulator_message, op_emulator_set_muted, op_emulator_set_volume, op_input_configure,
};
use super::input::{EventResult, KeyboardEvent, MouseEvent, op_event_result};
use super::modules::{BrowserModuleLoader, NoNpm, document_scripts, execute_document_script};
use super::state::{BridgeState, op_post_message, op_proxy_message};

const BOOTSTRAP: &str = include_str!("runtime.js");

deno_core::extension!(
    snowglass_game_layer,
    ops = [
        op_dom_create_element,
        op_dom_create_text,
        op_dom_create_comment,
        op_dom_append_child,
        op_dom_insert_before,
        op_dom_remove_child,
        op_dom_set_text,
        op_dom_set_attribute,
        op_dom_remove_attribute,
        op_post_message,
        op_proxy_message,
        op_event_result,
        op_emulator_launch,
        op_emulator_close,
        op_emulator_set_volume,
        op_emulator_set_muted,
        op_emulator_message,
        op_emulator_input,
        op_input_configure,
    ],
    esm_entry_point = "ext:snowglass_game_layer/bootstrap.js",
    esm = ["ext:snowglass_game_layer/bootstrap.js" = {
        source = r#"
                import { core } from "ext:core/mod.js";
                import {
                  op_dom_append_child,
                  op_dom_create_comment,
                  op_dom_create_element,
                  op_dom_create_text,
                  op_dom_insert_before,
                  op_dom_remove_attribute,
                  op_dom_remove_child,
                  op_dom_set_attribute,
                  op_dom_set_text,
                  op_event_result,
                  op_emulator_close,
                  op_emulator_input,
                  op_input_configure,
                  op_emulator_launch,
                  op_emulator_message,
                  op_emulator_set_muted,
                  op_emulator_set_volume,
                  op_post_message,
                  op_proxy_message,
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
                      op_dom_remove_attribute,
                      op_dom_remove_child,
                      op_dom_set_attribute,
                      op_dom_set_text,
                      op_event_result,
                      op_emulator_close,
                      op_emulator_input,
                      op_input_configure,
                      op_emulator_launch,
                      op_emulator_message,
                      op_emulator_set_muted,
                      op_emulator_set_volume,
                      op_post_message,
                      op_proxy_message,
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
    },
);

pub struct Runtime {
    worker: MainWorker,
    // V8's task runner schedules delayed foreground tasks through Tokio. Keep
    // this executor alive for exactly as long as the isolate and enter it for
    // every interaction with V8.
    tokio: tokio::runtime::Runtime,
}

impl Runtime {
    pub fn new(
        document: &BaseDocument,
        html: &str,
        source_name: &str,
        width: u32,
        height: u32,
        emulator_commands: EmulatorCommandSink,
        plugin_host: snowglass_pluginhost::PluginHost,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // This process also links WebRTC's ring-backed rustls feature, while
        // deno_runtime selects aws-lc-rs. With both providers compiled rustls
        // requires the embedder to choose one before fetch creates a client.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let tokio = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let tokio_context = tokio.enter();
        let document_url = ModuleSpecifier::parse(source_name)?;
        start_inspector_server()?;
        let browser_loader = Rc::new(BrowserModuleLoader::new());
        let module_loader = Rc::new(snowglass_pluginhost::PluginModuleRouter::new(
            plugin_host,
            browser_loader.clone(),
        ));
        let sys = sys_traits::impls::RealSys;
        let permissions = PermissionsContainer::allow_all(Arc::new(
            RuntimePermissionDescriptorParser::new(sys.clone()),
        ));
        let services: WorkerServiceOptions<NoNpm, NoNpm, sys_traits::impls::RealSys> =
            WorkerServiceOptions {
                blob_store: deno_runtime::deno_web::BlobStore::default_arc(),
                broadcast_channel: Default::default(),
                deno_rt_native_addon_loader: None,
                feature_checker: Arc::new(FeatureChecker::default()),
                fs: Arc::new(deno_runtime::deno_fs::RealFs),
                module_loader: module_loader.clone(),
                node_services: None,
                npm_process_state_provider: None,
                permissions,
                root_cert_store_provider: None,
                fetch_dns_resolver: Default::default(),
                shared_array_buffer_store: None,
                compiled_wasm_module_store: None,
                v8_code_cache: None,
                bundle_provider: None,
            };
        let bootstrap = BootstrapOptions {
            location: Some(document_url.clone()),
            inspect: true,
            user_agent: format!("Snowglass/{}", env!("CARGO_PKG_VERSION")),
            ..Default::default()
        };
        let mut worker = MainWorker::bootstrap_from_options(
            &document_url,
            services,
            WorkerOptions {
                bootstrap,
                extensions: vec![
                    snowglass_pluginhost::deno::extension(),
                    snowglass_game_layer::init(),
                ],
                ..Default::default()
            },
        );
        worker
            .js_runtime
            .op_state()
            .borrow_mut()
            .borrow_mut::<EmulatorBridge>()
            .command_sink = emulator_commands;
        worker
            .js_runtime
            .execute_script("snowglass:game-layer/deno/runtime.js", BOOTSTRAP)?;
        let snapshot = serde_json::to_string(&snapshot(document, width, height, None))?;
        worker.js_runtime.execute_script(
            "snowglass:game-layer/deno/document.js",
            format!("__snowglassDom.install({snapshot});"),
        )?;
        for (index, script) in document_scripts(html).enumerate() {
            let result = execute_document_script(
                &tokio,
                &mut worker,
                browser_loader.as_ref(),
                &document_url,
                index,
                script,
            );
            if let Err(error) = result {
                // A browser reports an individual script error without
                // discarding the document's realm or preventing later scripts.
                tracing::error!("snowglass: game-layer script {index} failed: {error}");
            }
        }
        drop(tokio_context);
        tokio.block_on(worker.run_up_to_duration(std::time::Duration::ZERO))?;
        Ok(Self { worker, tokio })
    }

    pub fn sync(
        &mut self,
        document: &BaseDocument,
        width: u32,
        height: u32,
        node_ids: &HashMap<usize, usize>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let _tokio_context = self.tokio.enter();
        let snapshot = serde_json::to_string(&snapshot(document, width, height, Some(node_ids)))?;
        self.worker.js_runtime.execute_script(
            "snowglass:game-layer/deno/sync.js",
            format!("__snowglassDom.sync({snapshot});"),
        )?;
        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), Box<dyn std::error::Error>> {
        let tokio_context = self.tokio.enter();
        self.worker.js_runtime.execute_script(
            "snowglass:game-layer/deno/resize.js",
            format!("__snowglassDom.resize({width}, {height});"),
        )?;
        drop(tokio_context);
        self.poll_event_loop()
    }

    pub fn dispatch_mouse(
        &mut self,
        kind: &str,
        target: usize,
        mouse: MouseEvent,
    ) -> Result<EventResult, Box<dyn std::error::Error>> {
        let tokio_context = self.tokio.enter();
        self.worker
            .js_runtime
            .op_state()
            .borrow_mut()
            .borrow_mut::<BridgeState>()
            .event = EventResult::default();
        let init = serde_json::to_string(&mouse)?;
        self.worker.js_runtime.execute_script(
            "snowglass:game-layer/deno/mouse-event.js",
            format!(
                "__snowglassDom.dispatchMouse({}, {target}, {init});",
                serde_json::to_string(kind)?
            ),
        )?;
        drop(tokio_context);
        self.poll_event_loop()?;
        Ok(self
            .worker
            .js_runtime
            .op_state()
            .borrow()
            .borrow::<BridgeState>()
            .event)
    }

    pub fn dispatch_keyboard(
        &mut self,
        kind: &str,
        keyboard: KeyboardEvent,
    ) -> Result<EventResult, Box<dyn std::error::Error>> {
        let tokio_context = self.tokio.enter();
        self.worker
            .js_runtime
            .op_state()
            .borrow_mut()
            .borrow_mut::<BridgeState>()
            .event = EventResult::default();
        let init = serde_json::to_string(&keyboard)?;
        self.worker.js_runtime.execute_script(
            "snowglass:game-layer/deno/keyboard-event.js",
            format!(
                "__snowglassDom.dispatchKeyboard({}, {init});",
                serde_json::to_string(kind)?
            ),
        )?;
        drop(tokio_context);
        self.poll_event_loop()?;
        Ok(self
            .worker
            .js_runtime
            .op_state()
            .borrow()
            .borrow::<BridgeState>()
            .event)
    }

    pub fn post_message(&mut self, message: &Value) -> Result<(), Box<dyn std::error::Error>> {
        let tokio_context = self.tokio.enter();
        self.worker.js_runtime.execute_script(
            "snowglass:game-layer/deno/message-event.js",
            format!(
                "__snowglassDom.dispatchMessage({});",
                serde_json::to_string(message)?
            ),
        )?;
        drop(tokio_context);
        self.poll_event_loop()?;
        Ok(())
    }

    pub fn proxy_request(&mut self, request: &Value) -> Result<(), Box<dyn std::error::Error>> {
        let tokio_context = self.tokio.enter();
        self.worker.js_runtime.execute_script(
            "snowglass:game-layer/deno/proxy-request.js",
            format!(
                "__snowglassDom.proxyRequest({});",
                serde_json::to_string(request)?
            ),
        )?;
        drop(tokio_context);
        self.poll_event_loop()?;
        Ok(())
    }

    pub fn dispatch_emulator_event(
        &mut self,
        event: &Value,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let tokio_context = self.tokio.enter();
        self.worker.js_runtime.execute_script(
            "snowglass:game-layer/deno/emulator-event.js",
            format!(
                "__snowglassDom.dispatchEmulatorEvent({});",
                serde_json::to_string(event)?
            ),
        )?;
        drop(tokio_context);
        self.poll_event_loop()?;
        Ok(())
    }

    pub fn set_input_configuration(
        &mut self,
        configuration: &Value,
        layouts: &Value,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let tokio_context = self.tokio.enter();
        self.worker.js_runtime.execute_script(
            "snowglass:game-layer/deno/input-configuration.js",
            format!(
                "__snowglassDom.setInputConfiguration({}, {});",
                serde_json::to_string(configuration)?,
                serde_json::to_string(layouts)?
            ),
        )?;
        drop(tokio_context);
        self.poll_event_loop()
    }

    pub fn set_input_state(
        &mut self,
        controllers: &Value,
        priority: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let tokio_context = self.tokio.enter();
        self.worker.js_runtime.execute_script(
            "snowglass:game-layer/deno/input-state.js",
            format!(
                "__snowglassDom.setInputState({}, {});",
                serde_json::to_string(controllers)?,
                serde_json::to_string(priority)?
            ),
        )?;
        drop(tokio_context);
        self.poll_event_loop()
    }

    pub fn input_configuration_result(
        &mut self,
        request_id: u64,
        result: &Value,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let tokio_context = self.tokio.enter();
        self.worker.js_runtime.execute_script(
            "snowglass:game-layer/deno/input-configuration-result.js",
            format!(
                "__snowglassDom.inputConfigurationResult({request_id}, {});",
                serde_json::to_string(result)?
            ),
        )?;
        drop(tokio_context);
        self.poll_event_loop()
    }

    /// Polls Deno once without waiting for the realm to become idle.
    ///
    /// A browser realm may keep timers, sockets, or an inspector session alive
    /// indefinitely. The shell's event loop calls this regularly so those
    /// tasks progress without blocking the window thread.
    pub fn poll_event_loop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.tokio
            .block_on(self.worker.run_up_to_duration(std::time::Duration::ZERO))?;
        Ok(())
    }

    pub fn take_mutations(&mut self) -> Vec<Mutation> {
        std::mem::take(
            &mut self
                .worker
                .js_runtime
                .op_state()
                .borrow_mut()
                .borrow_mut::<BridgeState>()
                .mutations,
        )
    }

    pub fn take_messages(&mut self) -> Vec<Value> {
        std::mem::take(
            &mut self
                .worker
                .js_runtime
                .op_state()
                .borrow_mut()
                .borrow_mut::<BridgeState>()
                .messages,
        )
    }

    pub fn take_proxy_messages(&mut self) -> Vec<Value> {
        std::mem::take(
            &mut self
                .worker
                .js_runtime
                .op_state()
                .borrow_mut()
                .borrow_mut::<BridgeState>()
                .proxy_messages,
        )
    }

    /// Invokes a registered emulator capability in this shell-owned Deno
    /// instance. Module routing follows the plugin module's referrer.
    pub fn run_emulator_plugin(
        &mut self,
        plugin: Arc<dyn snowglass_pluginhost::EmulatorPluginInstance>,
        arguments: serde_json::Value,
        configuration: snowglass_config::ast::ConfigurationInstance,
    ) -> Result<
        snowglass_pluginhost::EmulatorLaunchArguments,
        snowglass_pluginhost::EmulatorHostError,
    > {
        let mut launch = snowglass_pluginhost::EmulatorLaunch::attach(&mut self.worker.js_runtime);
        let result = self.tokio.block_on(launch.execute(
            snowglass_pluginhost::EmulatorSetupInvocation {
                plugin,
                arguments,
                configuration,
            },
        ));
        result
    }
}

fn start_inspector_server() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(test)]
    return Ok(());

    #[cfg(not(test))]
    static INSPECTOR: OnceLock<Result<std::net::SocketAddr, String>> = OnceLock::new();
    #[cfg(not(test))]
    match INSPECTOR.get_or_init(|| {
        let server = deno_runtime::deno_inspector_server::create_inspector_server(
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            "snowglass",
            deno_runtime::deno_inspector_server::InspectPublishUid {
                console: false,
                http: true,
            },
        )
        .map_err(|error| error.to_string())?;
        tracing::error!(
            "snowglass: game-layer DevTools targets: http://{}/json",
            server.host
        );
        Ok(server.host)
    }) {
        Ok(_) => Ok(()),
        Err(error) => Err(std::io::Error::other(error.clone()).into()),
    }
}
