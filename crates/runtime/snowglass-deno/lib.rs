//! A deliberately small Deno-compatible web runtime for Snowglass.
//!
//! This crate builds on [`deno_core`] and selects only the browser primitives
//! used by Snowglass documents. It intentionally does not include the Deno
//! namespace, Node compatibility, WebCrypto, filesystem, process, FFI, KV,
//! NAPI, workers, or other services bundled by `deno_runtime::MainWorker`.

use deno_core::{
    Extension, JsRuntime, ModuleId, ModuleLoader, ModuleSpecifier, PollEventLoopOptions,
    RuntimeOptions as CoreRuntimeOptions,
};
use deno_permissions::{PermissionsContainer, RuntimePermissionDescriptorParser};
use std::{path::PathBuf, rc::Rc, sync::Arc, time::Duration};

pub use deno_inspector_server::{InspectPublishUid, InspectorServerError, create_inspector_server};

deno_core::extension!(
    snowglass_web_runtime,
    esm_entry_point = "ext:snowglass_web_runtime/bootstrap.js",
    esm = [dir ".", "bootstrap.js"],
);

/// Options for a minimal Snowglass JavaScript runtime.
pub struct RuntimeOptions {
    /// URL used for location-sensitive web APIs and inspector presentation.
    pub main_module: ModuleSpecifier,
    /// Loader for application modules. The runtime itself supplies only `ext:`
    /// modules from its selected extensions.
    pub module_loader: Rc<dyn ModuleLoader>,
    /// Host extensions appended after the minimal web platform is installed.
    pub extensions: Vec<Extension>,
    /// Value used by the Fetch implementation for outgoing requests.
    pub user_agent: String,
    /// Directory for persistent `localStorage`. With `None`, the global is
    /// installed but reports that persistent storage is unavailable; ephemeral
    /// `sessionStorage` remains available.
    pub storage_dir: Option<PathBuf>,
    /// Registers this isolate with an already-created Deno inspector server.
    pub inspect: bool,
}

/// A `deno_core` isolate with a curated browser-like global scope.
pub struct Runtime {
    js_runtime: JsRuntime,
}

impl Runtime {
    pub fn new(mut options: RuntimeOptions) -> Self {
        let location = options.main_module.clone();
        let mut extensions = vec![
            deno_webidl::deno_webidl::init(),
            deno_web::deno_web::init(
                deno_web::BlobStore::default_arc(),
                Some(location),
                true,
                Default::default(),
            ),
            deno_fetch::deno_fetch::init(deno_fetch::Options {
                user_agent: options.user_agent,
                ..Default::default()
            }),
            deno_websocket::deno_websocket::init(),
            deno_webstorage::deno_webstorage::init(options.storage_dir),
            snowglass_web_runtime::init(),
        ];
        extensions.append(&mut options.extensions);

        let js_runtime = JsRuntime::new(CoreRuntimeOptions {
            module_loader: Some(options.module_loader),
            extensions,
            inspector: options.inspect,
            is_main: true,
            ..Default::default()
        });
        js_runtime
            .op_state()
            .borrow_mut()
            .put(PermissionsContainer::allow_all(Arc::new(
                RuntimePermissionDescriptorParser::new(sys_traits::impls::RealSys),
            )));

        if options.inspect
            && let Some(server) = deno_inspector_server::get_inspector_server()
        {
            let inspector_url = server.register_inspector(
                options.main_module.to_string(),
                js_runtime.inspector(),
                false,
            );
            js_runtime.op_state().borrow_mut().put(inspector_url);
        }

        Self { js_runtime }
    }

    pub fn js_runtime(&self) -> &JsRuntime {
        &self.js_runtime
    }

    pub fn js_runtime_mut(&mut self) -> &mut JsRuntime {
        &mut self.js_runtime
    }

    pub async fn execute_side_module(
        &mut self,
        specifier: &ModuleSpecifier,
    ) -> Result<(), deno_core::error::CoreError> {
        let id = self.js_runtime.load_side_es_module(specifier).await?;
        self.evaluate_module(id).await
    }

    async fn evaluate_module(&mut self, id: ModuleId) -> Result<(), deno_core::error::CoreError> {
        let mut evaluation = self.js_runtime.mod_evaluate(id);
        tokio::select! {
            biased;
            result = &mut evaluation => result,
            result = self.js_runtime.run_event_loop(PollEventLoopOptions::default()) => {
                result?;
                evaluation.await
            }
        }
    }

    /// Polls the isolate at least once, returning when it becomes idle or the
    /// supplied duration elapses.
    pub async fn run_up_to_duration(
        &mut self,
        duration: Duration,
    ) -> Result<(), deno_core::error::CoreError> {
        match tokio::time::timeout(
            duration,
            self.js_runtime
                .run_event_loop(PollEventLoopOptions::default()),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deno_core::{ModuleLoadResponse, ModuleSource, ResolutionKind, resolve_import};

    struct EmptyLoader;

    impl ModuleLoader for EmptyLoader {
        fn resolve(
            &self,
            specifier: &str,
            referrer: &str,
            _kind: ResolutionKind,
        ) -> Result<ModuleSpecifier, deno_core::error::ModuleLoaderError> {
            resolve_import(specifier, referrer)
                .map_err(deno_core::error::ModuleLoaderError::from_err)
        }

        fn load(
            &self,
            _module_specifier: &ModuleSpecifier,
            _maybe_referrer: Option<&deno_core::ModuleLoadReferrer>,
            _options: deno_core::ModuleLoadOptions,
        ) -> ModuleLoadResponse {
            ModuleLoadResponse::Sync(Err(deno_core::error::ModuleLoaderError::generic(
                "no application modules are configured",
            )))
        }
    }

    fn runtime() -> Runtime {
        Runtime::new(RuntimeOptions {
            main_module: ModuleSpecifier::parse("https://snowglass.invalid/runtime.js").unwrap(),
            module_loader: Rc::new(EmptyLoader),
            extensions: Vec::new(),
            user_agent: "Snowglass/Test".to_owned(),
            storage_dir: None,
            inspect: false,
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exposes_selected_web_apis_without_deno_or_crypto() {
        let mut runtime = runtime();
        let value = runtime
            .js_runtime_mut()
            .execute_script(
                "snowglass:test/globals.js",
                r#"JSON.stringify({
                    event: typeof EventTarget === "function",
                    url: new URL("child", "https://example.com/root/").href,
                    channel: typeof MessageChannel === "function",
                    timer: typeof setTimeout === "function",
                    fetch: typeof fetch === "function",
                    websocket: typeof WebSocket === "function",
                    storage: typeof Storage === "function" && typeof sessionStorage === "object",
                    deno: typeof Deno,
                    process: typeof process,
                    crypto: typeof crypto,
                })"#,
            )
            .unwrap();
        let scope = &mut runtime.js_runtime_mut().handle_scope();
        let value = deno_core::v8::Local::new(scope, value);
        let value = value.to_rust_string_lossy(scope);
        assert_eq!(
            value,
            r#"{"event":true,"url":"https://example.com/root/child","channel":true,"timer":true,"fetch":true,"websocket":true,"storage":true,"deno":"undefined","process":"undefined","crypto":"undefined"}"#
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drives_fetch_and_timers() {
        let mut runtime = runtime();
        runtime
            .js_runtime_mut()
            .execute_script(
                "snowglass:test/async.js",
                r#"
                    globalThis.result = [];
                    fetch("data:text/plain,ready")
                      .then((response) => response.text())
                      .then((text) => result.push(text));
                    setTimeout(() => result.push("timer"), 1);
                "#,
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        runtime.run_up_to_duration(Duration::ZERO).await.unwrap();
        let value = runtime
            .js_runtime_mut()
            .execute_script("snowglass:test/result.js", "JSON.stringify(result)")
            .unwrap();
        let scope = &mut runtime.js_runtime_mut().handle_scope();
        let value = deno_core::v8::Local::new(scope, value);
        assert_eq!(value.to_rust_string_lossy(scope), r#"["ready","timer"]"#);
    }
}
