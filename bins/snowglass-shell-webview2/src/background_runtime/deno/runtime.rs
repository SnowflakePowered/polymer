use std::collections::HashMap;
use std::rc::Rc;
#[cfg(not(test))]
use std::sync::OnceLock;

use blitz_dom::BaseDocument;
use deno_core::{ModuleSpecifier, OpState, op2};
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
use super::modules::{BrowserModuleLoader, document_scripts, execute_document_script};
use super::state::{BridgeState, op_post_message, op_proxy_message};

const BOOTSTRAP: &str = include_str!("runtime.js");
const EVENT_DISPATCH: &str = include_str!("event_dispatch.js");
const INPUT_FACADE: &str = include_str!("input_facade.js");
const EMULATOR_FACADE: &str = include_str!("emulator_facade.js");
const SHARED_BUS_CLIENT: &str =
    include_str!("../../../../../crates/runtime/snowglass-pluginhost/bus_client.js");
const PLUGIN_FACADE: &str = include_str!("../../js/plugin_facade.js");
const REMOTE_VALUE_CODEC: &str = include_str!("../../js/remote_value_codec.js");

#[derive(Default)]
struct SharedBusBridge(Option<crate::shared_bus::BackgroundBusEndpoint>);

#[op2(fast)]
fn op_shared_bus_doorbell(state: &mut OpState) {
    if let Some(endpoint) = state.borrow::<SharedBusBridge>().0.as_ref() {
        endpoint.wake_native();
    }
}

deno_core::extension!(
    snowglass_background_runtime,
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
        op_shared_bus_doorbell,
    ],
    esm_entry_point = "ext:snowglass_background_runtime/bootstrap.js",
    esm = ["ext:snowglass_background_runtime/bootstrap.js" = {
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
                  op_shared_bus_doorbell,
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
                Object.defineProperty(globalThis, "__snowglassSharedBusDoorbell", {
                  value: op_shared_bus_doorbell,
                  configurable: false,
                });
            "#
    },],
    state = |state| {
        state.put(BridgeState::default());
        state.put(EmulatorBridge::default());
        state.put(SharedBusBridge::default());
    },
);

pub struct Runtime {
    worker: snowglass_deno::Runtime,
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
            None,
        )
        .await
    }

    #[cfg(test)]
    pub async fn evaluate_for_test(
        &mut self,
        source: &'static str,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let promise = self
            .worker
            .js_runtime_mut()
            .execute_script("snowglass:test/evaluate.js", source)?;
        #[allow(deprecated)]
        let result = self.worker.js_runtime_mut().resolve_value(promise).await?;
        deno_core::scope!(scope, self.worker.js_runtime_mut());
        let result = deno_core::v8::Local::new(scope, result);
        Ok(deno_core::serde_v8::from_v8(scope, result)?)
    }

    #[cfg(test)]
    pub fn install_shared_bus_client_for_test(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.worker.js_runtime_mut().execute_script(
            "snowglass:test/shared-bus-adapter.js",
            r#"
              const __testLaneCapacity = 4096;
              const __testBusBuffer = new SharedArrayBuffer(4 * (64 + __testLaneCapacity));
              for (let lane = 0; lane < 4; lane++) {
                const base = lane * (64 + __testLaneCapacity);
                const view = new DataView(__testBusBuffer, base, 64);
                view.setUint32(0, 0x31424753, true);
                view.setUint16(4, 1, true);
                view.setUint16(6, lane, true);
                view.setUint32(8, 1, true);
                view.setUint32(12, __testLaneCapacity, true);
              }
              const __testBusListeners = { sharedbufferreceived: [], message: [] };
              Object.defineProperty(globalThis, "chrome", {
                value: { webview: Object.freeze({
                  addEventListener(type, listener) { __testBusListeners[type]?.push(listener); },
                  releaseBuffer() {},
                }) },
                configurable: true,
              });
              Object.defineProperty(globalThis, "ipc", {
                value: Object.freeze({ postMessage() {} }),
                configurable: true,
              });
              Object.defineProperty(globalThis, "__snowglassBusLanes", {
                value: [2, 3], configurable: true,
              });
              Object.defineProperty(globalThis, "__attachTestBus", {
                value() {
                  for (const listener of __testBusListeners.sharedbufferreceived) {
                    listener({ getBuffer() { return __testBusBuffer; } });
                  }
                },
              });
            "#,
        )?;
        self.worker
            .js_runtime_mut()
            .execute_script("snowglass:test/shared-bus-client.js", SHARED_BUS_CLIENT)?;
        self.worker
            .js_runtime_mut()
            .execute_script("snowglass:test/shared-bus-attach.js", "__attachTestBus();")?;
        Ok(())
    }

    pub(in crate::background_runtime) fn serialized_snapshot(
        document: &BaseDocument,
        width: u32,
        height: u32,
    ) -> Result<String, serde_json::Error> {
        serde_json::to_string(&snapshot(document, width, height, None))
    }

    pub(in crate::background_runtime) async fn from_snapshot(
        initial_snapshot: String,
        html: String,
        source_name: String,
        emulator_commands: EmulatorCommandSink,
        background_bus: Option<crate::shared_bus::BackgroundBusEndpoint>,
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
            extensions: vec![snowglass_background_runtime::init()],
            user_agent: format!("Snowglass/{}", env!("CARGO_PKG_VERSION")),
            storage_dir,
            inspect: true,
        });
        if let Some(background_bus) = background_bus {
            background_bus
                .install(worker.js_runtime_mut())
                .map_err(std::io::Error::other)?;
            worker
                .js_runtime_mut()
                .op_state()
                .borrow_mut()
                .borrow_mut::<SharedBusBridge>()
                .0 = Some(background_bus);
            worker.js_runtime_mut().execute_script(
                "snowglass:shared-bus/deno-adapter.js",
                r#"
                  const __snowglassBusListeners = { sharedbufferreceived: [], message: [] };
                  Object.defineProperty(globalThis, "chrome", {
                    value: { webview: Object.freeze({
                      addEventListener(type, listener) { __snowglassBusListeners[type]?.push(listener); },
                      releaseBuffer() {},
                    }) },
                  });
                  Object.defineProperty(globalThis, "ipc", {
                    value: Object.freeze({ postMessage() { __snowglassSharedBusDoorbell(); } }),
                  });
                  Object.defineProperty(globalThis, "__snowglassBusLanes", { value: [2, 3] });
                  Object.defineProperty(globalThis, "__snowglassDispatchBusBuffer", {
                    value() {
                      for (const listener of __snowglassBusListeners.sharedbufferreceived) {
                        listener({ getBuffer() { return __snowglassBusBuffer; } });
                      }
                    },
                  });
                "#,
            )?;
            worker
                .js_runtime_mut()
                .execute_script("snowglass:shared-bus/client.js", SHARED_BUS_CLIENT)?;
            worker.js_runtime_mut().execute_script(
                "snowglass:shared-bus/attach.js",
                "__snowglassDispatchBusBuffer();",
            )?;
        }
        worker
            .js_runtime_mut()
            .execute_script("snowglass:pluginhost/facade.js", PLUGIN_FACADE)?;
        worker
            .js_runtime_mut()
            .execute_script("snowglass:bridge/remote-value-codec.js", REMOTE_VALUE_CODEC)?;
        worker
            .js_runtime_mut()
            .execute_script("snowglass:background/event-dispatch.js", EVENT_DISPATCH)?;
        worker
            .js_runtime_mut()
            .op_state()
            .borrow_mut()
            .borrow_mut::<EmulatorBridge>()
            .command_sink = emulator_commands;
        worker
            .js_runtime_mut()
            .execute_script("snowglass:background/input-facade.js", INPUT_FACADE)?;
        worker
            .js_runtime_mut()
            .execute_script("snowglass:background/emulator-facade.js", EMULATOR_FACADE)?;
        worker
            .js_runtime_mut()
            .execute_script("snowglass:background/runtime.js", BOOTSTRAP)?;
        worker.js_runtime_mut().execute_script(
            "snowglass:background/document.js",
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
                tracing::error!("snowglass: background script {index} failed: {error}");
            }
        }
        worker.run_up_to_duration(std::time::Duration::ZERO).await?;
        Ok(Self { worker })
    }

    pub fn sync(
        &mut self,
        document: &BaseDocument,
        width: u32,
        height: u32,
        node_ids: &HashMap<usize, usize>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = serde_json::to_string(&snapshot(document, width, height, Some(node_ids)))?;
        self.worker.js_runtime_mut().execute_script(
            "snowglass:background/sync.js",
            format!("__snowglassDom.sync({snapshot});"),
        )?;
        Ok(())
    }

    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), Box<dyn std::error::Error>> {
        self.worker.js_runtime_mut().execute_script(
            "snowglass:background/resize.js",
            format!("__snowglassDom.resize({width}, {height});"),
        )?;
        Ok(())
    }

    pub fn dispatch_mouse(
        &mut self,
        kind: &str,
        target: usize,
        mouse: MouseEvent,
    ) -> Result<EventResult, Box<dyn std::error::Error>> {
        self.worker
            .js_runtime_mut()
            .op_state()
            .borrow_mut()
            .borrow_mut::<BridgeState>()
            .event = EventResult::default();
        let init = serde_json::to_string(&mouse)?;
        self.worker.js_runtime_mut().execute_script(
            "snowglass:background/mouse-event.js",
            format!(
                "__snowglassDom.dispatchMouse({}, {target}, {init});",
                serde_json::to_string(kind)?
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

    pub fn dispatch_keyboard(
        &mut self,
        kind: &str,
        keyboard: KeyboardEvent,
    ) -> Result<EventResult, Box<dyn std::error::Error>> {
        self.worker
            .js_runtime_mut()
            .op_state()
            .borrow_mut()
            .borrow_mut::<BridgeState>()
            .event = EventResult::default();
        let init = serde_json::to_string(&keyboard)?;
        self.worker.js_runtime_mut().execute_script(
            "snowglass:background/keyboard-event.js",
            format!(
                "__snowglassDom.dispatchKeyboard({}, {init});",
                serde_json::to_string(kind)?
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

    pub fn post_message(&mut self, message: &Value) -> Result<(), Box<dyn std::error::Error>> {
        self.worker.js_runtime_mut().execute_script(
            "snowglass:background/message-event.js",
            format!(
                "__snowglassDom.dispatchMessage({});",
                serde_json::to_string(message)?
            ),
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub fn proxy_request(&mut self, request: &Value) -> Result<(), Box<dyn std::error::Error>> {
        self.worker.js_runtime_mut().execute_script(
            "snowglass:test/proxy-request.js",
            format!(
                "__snowglassDom.proxyRequest({});",
                serde_json::to_string(request)?
            ),
        )?;
        Ok(())
    }

    pub fn dispatch_emulator_event(
        &mut self,
        event: &Value,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.worker.js_runtime_mut().execute_script(
            "snowglass:background/emulator-event.js",
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
            "snowglass:background/input-configuration.js",
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
            "snowglass:background/input-state.js",
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
            "snowglass:background/input-configuration-result.js",
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
        if self
            .worker
            .js_runtime()
            .op_state()
            .borrow()
            .borrow::<SharedBusBridge>()
            .0
            .is_some()
        {
            self.worker.js_runtime_mut().execute_script(
                "snowglass:shared-bus/doorbell.js",
                "__snowglassSharedBusNotify?.();",
            )?;
        }
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

    #[cfg(test)]
    pub fn take_proxy_messages(&mut self) -> Vec<Value> {
        std::mem::take(
            &mut self
                .worker
                .js_runtime_mut()
                .op_state()
                .borrow_mut()
                .borrow_mut::<BridgeState>()
                .proxy_messages,
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
            "snowglass: background DevTools targets: http://{}/json",
            server.host
        );
        Ok(server.host)
    }) {
        Ok(_) => Ok(()),
        Err(error) => Err(std::io::Error::other(error.clone()).into()),
    }
}
