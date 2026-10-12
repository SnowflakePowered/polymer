use std::rc::Rc;

use blitz_dom::BaseDocument;
use deno_core::ModuleSpecifier;
use serde_json::Value;

use super::dom::{
    LayoutCache, LayoutSnapshot, ScriptNodeId, op_dom_append_child, op_dom_attributes,
    op_dom_clear_image_mip_chains,
    op_dom_children, op_dom_clone, op_dom_closest, op_dom_computed_style, op_dom_create_comment,
    op_dom_create_element,
    op_dom_create_fragment, op_dom_create_text, op_dom_element_by_id, op_dom_focus, op_dom_focused,
    op_dom_get_attribute, op_dom_image_size, op_dom_insert_before, op_dom_is_connected,
    op_dom_matches, op_dom_node_info, op_dom_query, op_dom_rect, op_dom_relative,
    op_dom_remove_attribute, op_dom_remove_child, op_dom_scroll_metrics, op_dom_set_attribute,
    op_dom_set_inner_html, op_dom_set_text, op_dom_take_layout, op_dom_template_content,
    op_dom_text, op_dom_text_content,
};
use super::assets::{op_shell_read_asset, op_shell_resolve_asset};
use super::input::{EventResult, op_event_result};
use super::media::{
    op_media_configure, op_media_load, op_media_pause, op_media_play, op_media_seek,
    op_media_state,
};
use super::modules::{BrowserModuleLoader, document_scripts, execute_document_script};
use super::scroll::op_dom_scroll;
use super::shaders::{
    op_shader_attach, op_shader_attached, op_shader_detach, op_shader_parameter, op_shader_passes,
    op_shader_preset_drop, op_shader_preset_load,
};
use super::state::{BridgeState, op_post_message, op_shell_reload};
use crate::shell::document::SharedShellDocument;
use polymer_deno::polymer::PolymerNotificationSender;

const BOOTSTRAP: &str = include_str!("runtime.js");
const EVENT_DISPATCH: &str = include_str!("event_dispatch.js");

deno_core::extension!(
    polymer_shell_runtime,
    ops = [
        op_dom_node_info,
        op_dom_relative,
        op_dom_children,
        op_dom_text,
        op_dom_text_content,
        op_dom_is_connected,
        op_dom_attributes,
        op_dom_get_attribute,
        op_dom_computed_style,
        op_dom_query,
        op_dom_matches,
        op_dom_closest,
        op_dom_element_by_id,
        op_dom_focused,
        op_dom_rect,
        op_dom_scroll_metrics,
        op_dom_image_size,
        op_dom_clear_image_mip_chains,
        op_dom_create_element,
        op_dom_create_text,
        op_dom_create_comment,
        op_dom_create_fragment,
        op_dom_template_content,
        op_dom_clone,
        op_dom_set_inner_html,
        op_dom_append_child,
        op_dom_insert_before,
        op_dom_remove_child,
        op_dom_set_text,
        op_dom_set_attribute,
        op_dom_remove_attribute,
        op_dom_focus,
        op_dom_scroll,
        op_dom_take_layout,
        op_media_load,
        op_media_play,
        op_media_pause,
        op_media_seek,
        op_media_configure,
        op_media_state,
        op_shader_preset_load,
        op_shader_attach,
        op_shader_attached,
        op_shader_preset_drop,
        op_shader_parameter,
        op_shader_passes,
        op_shader_detach,
        op_post_message,
        op_shell_reload,
        op_shell_resolve_asset,
        op_shell_read_asset,
        op_event_result,
    ],
    esm_entry_point = "ext:polymer_shell_runtime/bootstrap.js",
    esm = ["ext:polymer_shell_runtime/bootstrap.js" = {
        source = r#"
                import { core } from "ext:core/mod.js";
                const webEvents = core.loadExtScript("ext:deno_web/02_event.js");
                core.setReportExceptionCallback((error) => {
                  const event = new webEvents.ErrorEvent("error", {
                    cancelable: true,
                    error,
                    message: String(error?.message ?? error),
                  });
                  if (globalThis.dispatchEvent(event)) console.error("Uncaught", error);
                });
                core.setUnhandledPromiseRejectionHandler((_promise, reason) => {
                  console.error("Uncaught (in promise)", reason);
                  return true;
                });
                Object.defineProperty(globalThis, "__polymerRuntime", {
                  value: Object.freeze({
                    runMicrotasks: core.runMicrotasks,
                    ops: core.ops,
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
    worker: polymer_deno::Runtime,
    inspector_address: std::net::SocketAddr,
    layout_cache: LayoutCache,
    layout_view_state: Option<(u32, u32, u32)>,
}

impl Runtime {
    pub(in crate::shell::runtime) async fn start(
        debug: bool,
        document: SharedShellDocument,
        html: String,
        source_name: String,
        commands: polymer_runtime::FrontendCommands,
        plugin_runtime: std::sync::Arc<polymer_pluginhost::runtime::PluginRuntime>,
        local_storage: Option<polymer_deno::webstorage::LocalStorage>,
        library: Option<polymer_runtime::LibraryService>,
        input_sources: polymer_input::InputHub,
        controller_api: polymer_runtime::ControllerApiState,
    ) -> Result<(Self, PolymerNotificationSender), Box<dyn std::error::Error>> {
        // This process also links WebRTC's ring-backed rustls feature, while
        // the selected Deno fetch/websocket extensions use aws-lc-rs. With both providers compiled rustls
        // requires the embedder to choose one before fetch creates a client.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let inspector_address = Self::start_inspector_server()?;
        let install = {
            let shell = document.0.borrow();
            let (base, width, height, scale_factor) = shell.runtime_context();
            format!(
                "__polymerDom.install({}, {width}, {height}, {scale_factor});",
                base.root_node().id.to_script()
            )
        };
        let document_url = ModuleSpecifier::parse(&source_name)?;
        let browser_loader = Rc::new(BrowserModuleLoader::new());
        let (polymer_extension, notifications) =
            polymer_deno::polymer::init_with_notifications(|notifications| {
                polymer_runtime::FrontendServices::new(
                    plugin_runtime.clone(),
                    Some(polymer_runtime::ConfigurationDefaults::default()),
                    library.clone(),
                    input_sources.clone(),
                    controller_api.clone(),
                    commands,
                    notifications,
                )
            });
        let mut worker = polymer_deno::Runtime::new(polymer_deno::RuntimeOptions {
            main_module: document_url.clone(),
            module_loader: browser_loader.clone(),
            extensions: vec![polymer_extension, polymer_shell_runtime::init()],
            user_agent: format!("Polymer/{}", env!("CARGO_PKG_VERSION")),
            local_storage,
            inspect: true,
            insecure_hosts: Vec::new(),
        });
        worker
            .js_runtime_mut()
            .op_state()
            .borrow_mut()
            .put(document);
        worker.js_runtime_mut().execute_script(
            "polymer:shell/meta.js",
            format!("Polymer.meta.debug = {debug}; Object.freeze(Polymer.meta);"),
        )?;
        worker
            .js_runtime_mut()
            .execute_script("polymer:shell/event-dispatch.js", EVENT_DISPATCH)?;
        worker
            .js_runtime_mut()
            .execute_script("polymer:shell/runtime.js", BOOTSTRAP)?;
        worker
            .js_runtime_mut()
            .execute_script("polymer:shell/document.js", install)?;
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
                tracing::error!("polymer: shell script {index} failed: {error}");
            }
        }
        worker.run_up_to_duration(std::time::Duration::ZERO).await?;
        Ok((
            Self {
                worker,
                inspector_address,
                layout_cache: LayoutCache::new(),
                layout_view_state: None,
            },
            notifications,
        ))
    }

    pub fn stage_layout(
        &mut self,
        document: &BaseDocument,
        width: u32,
        height: u32,
        device_pixel_ratio: f32,
    ) {
        let layout = LayoutSnapshot::capture(
            document,
            width,
            height,
            device_pixel_ratio,
            &mut self.layout_cache,
        );
        let view_state = layout.view_state();
        if !layout.has_changes() && self.layout_view_state == Some(view_state) {
            return;
        }
        self.layout_view_state = Some(view_state);
        let worker = &mut self.worker;
        worker
            .js_runtime_mut()
            .op_state()
            .borrow_mut()
            .borrow_mut::<BridgeState>()
            .layout = Some(layout);
    }

    pub fn publish_layout(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let worker = &mut self.worker;
        if worker
            .js_runtime_mut()
            .op_state()
            .borrow()
            .borrow::<BridgeState>()
            .layout
            .is_none()
        {
            return Ok(());
        }
        worker
            .js_runtime_mut()
            .execute_script("polymer:shell/sync.js", "__polymerDom.syncLayout();")?;
        Ok(())
    }

    pub fn resize(
        &mut self,
        width: u32,
        height: u32,
        device_pixel_ratio: f32,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let worker = &mut self.worker;
        worker.js_runtime_mut().execute_script(
            "polymer:shell/resize.js",
            format!("__polymerDom.resize({width}, {height}, {device_pixel_ratio});"),
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
        let worker = &mut self.worker;
        worker
            .js_runtime_mut()
            .op_state()
            .borrow_mut()
            .borrow_mut::<BridgeState>()
            .event = EventResult::default();
        worker.js_runtime_mut().execute_script(
            "polymer:shell/dom-event.js",
            format!(
                "__polymerDom.dispatchDom({}, {target}, {}, {}, {bubbles}, {cancelable});",
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

    /// Advances timers, sockets, promises and inspector traffic, registering
    /// the shell thread's waker for whichever of them completes next.
    pub fn poll_event_loop(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), deno_core::error::CoreError>> {
        self.worker
            .js_runtime_mut()
            .poll_event_loop(cx, deno_core::PollEventLoopOptions::default())
    }

    pub fn reload_requested(&self) -> bool {
        self.worker
            .js_runtime()
            .op_state()
            .borrow()
            .borrow::<BridgeState>()
            .reload
    }

    pub(crate) const fn inspector_address(&self) -> std::net::SocketAddr {
        self.inspector_address
    }

    fn start_inspector_server() -> Result<std::net::SocketAddr, Box<dyn std::error::Error>> {
        let server = polymer_deno::create_inspector_server(
            std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            "polymer",
            polymer_deno::InspectPublishUid {
                console: false,
                http: true,
            },
        )?;
        tracing::info!(
            "polymer: shell DevTools targets: http://{}/json",
            server.host
        );
        Ok(server.host)
    }
}
