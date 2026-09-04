//! A deliberately small Deno-compatible web runtime for Polymer.
//!
//! This crate builds on [`deno_core`] and selects only the browser primitives
//! used by Polymer documents. It intentionally does not include the Deno
//! namespace, Node compatibility, WebCrypto, filesystem, process, FFI, KV,
//! NAPI, workers, or other services bundled by `deno_runtime::MainWorker`.

use deno_core::{
    Extension, JsRuntime, ModuleCodeString, ModuleId, ModuleLoader, ModuleName, ModuleSpecifier,
    OpState, PollEventLoopOptions, RuntimeOptions as CoreRuntimeOptions, SourceMapData, op2,
};
use deno_permissions::{PermissionsContainer, RuntimePermissionDescriptorParser};
use std::{path::Path, rc::Rc, sync::Arc, time::Duration};

pub mod polymer;
pub mod webstorage;

pub use deno_inspector_server::{InspectPublishUid, InspectorServerError, create_inspector_server};

struct UserAgent(String);

#[op2]
#[string]
fn op_polymer_user_agent(state: &mut OpState) -> String {
    state.borrow::<UserAgent>().0.clone()
}

#[op2]
#[string]
fn op_polymer_location(state: &mut OpState) -> String {
    state.borrow::<deno_web::Location>().0.to_string()
}

deno_core::extension!(polymer_web_runtime,
    ops = [op_polymer_location, op_polymer_user_agent],
    esm_entry_point = "ext:polymer_web_runtime/98_web_runtime.js",
    esm = [dir "js", "98_web_runtime.js"],
    options = {
        user_agent: String,
    },
    state = |state, options| {
        state.put(UserAgent(options.user_agent));
    },
);

deno_error::js_error_wrapper!(deno_ast::ParseDiagnostic, JsParseDiagnostic, "Error");
deno_error::js_error_wrapper!(deno_ast::TranspileError, JsTranspileError, "Error");

fn transpile_extension_source(
    name: ModuleName,
    source: ModuleCodeString,
) -> Result<(ModuleCodeString, Option<SourceMapData>), deno_error::JsErrorBox> {
    use deno_ast::{
        EmitOptions, ImportsNotUsedAsValues, MediaType, ParseParams, SourceMapOption,
        TranspileModuleOptions, TranspileOptions,
    };

    let media_type = MediaType::from_path(Path::new(&name));
    if matches!(media_type, MediaType::JavaScript | MediaType::Mjs) {
        return Ok((source, None));
    }
    assert_eq!(media_type, MediaType::TypeScript);

    let parsed = deno_ast::parse_module(ParseParams {
        specifier: deno_core::url::Url::parse(&name).unwrap(),
        text: source.into(),
        media_type,
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })
    .map_err(|error| deno_error::JsErrorBox::from_err(JsParseDiagnostic(error)))?;
    let transpiled = parsed
        .transpile(
            &TranspileOptions {
                imports_not_used_as_values: ImportsNotUsedAsValues::Remove,
                ..Default::default()
            },
            &TranspileModuleOptions::default(),
            &EmitOptions {
                source_map: SourceMapOption::Separate,
                ..Default::default()
            },
        )
        .map_err(|error| deno_error::JsErrorBox::from_err(JsTranspileError(error)))?
        .into_source();

    Ok((
        transpiled.text.into(),
        transpiled.source_map.map(|map| map.into_bytes().into()),
    ))
}

/// Options for a minimal Polymer JavaScript runtime.
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
    /// Persistent storage exposed through the asynchronous `localStorage` API.
    /// With `None`, the global reports that persistent storage is unavailable;
    /// asynchronous, ephemeral `sessionStorage` remains available.
    pub local_storage: Option<webstorage::LocalStorage>,
    /// Registers this isolate with an already-created Deno inspector server.
    pub inspect: bool,
}

/// A `deno_core` isolate with a curated browser-like global scope.
pub struct Runtime {
    js_runtime: JsRuntime,
}

impl Runtime {
    pub fn new(mut options: RuntimeOptions) -> Self {
        // A workspace embedder can unify rustls dependencies that select both
        // built-in providers. Pick the provider used by Deno's networking
        // extensions before they lazily construct a TLS client.
        let _ = deno_tls::rustls::crypto::aws_lc_rs::default_provider().install_default();
        let location = options.main_module.clone();
        let user_agent = options.user_agent;
        let mut extensions = vec![
            deno_telemetry::deno_telemetry::init(),
            deno_webidl::deno_webidl::init(),
            deno_web::deno_web::init(
                deno_web::BlobStore::default_arc(),
                Some(location),
                true,
                Default::default(),
            ),
            deno_net::deno_net::init(None, None),
            deno_tls::deno_tls::init(),
            deno_fetch::deno_fetch::init(deno_fetch::Options {
                user_agent: user_agent.clone(),
                ..Default::default()
            }),
            deno_websocket::deno_websocket::init(),
            webstorage::extension(options.local_storage),
            polymer_web_runtime::init(user_agent),
        ];
        extensions.append(&mut options.extensions);

        let js_runtime = JsRuntime::new(CoreRuntimeOptions {
            module_loader: Some(options.module_loader),
            extensions,
            extension_transpiler: Some(Rc::new(transpile_extension_source)),
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
        js_runtime
            .op_state()
            .borrow_mut()
            .put(Arc::new(deno_features::FeatureChecker::default()));

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
    use deno_core::{ModuleLoadResponse, ResolutionKind, resolve_import};

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
            main_module: ModuleSpecifier::parse("https://polymer.invalid/runtime.js").unwrap(),
            module_loader: Rc::new(EmptyLoader),
            extensions: Vec::new(),
            user_agent: "Polymer/Test".to_owned(),
            local_storage: None,
            inspect: false,
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn exposes_selected_web_apis_without_deno_or_crypto() {
        let mut runtime = runtime();
        let value = runtime
            .js_runtime_mut()
            .execute_script(
                "polymer:test/globals.js",
                r#"(() => {
                  let dispatched = false;
                  addEventListener("polymer-test", () => dispatched = true);
                  dispatchEvent(new Event("polymer-test"));
                  return JSON.stringify({
                    event: typeof EventTarget === "function" && dispatched,
                    url: new URL("child", "https://example.com/root/").href,
                    location: location instanceof Location &&
                      location.href === "https://polymer.invalid/runtime.js" &&
                      location.origin === "https://polymer.invalid" &&
                      location.pathname === "/runtime.js",
                    channel: typeof MessageChannel === "function",
                    timer: typeof setTimeout === "function",
                    fetch: typeof fetch === "function",
                    websocket: typeof WebSocket === "function",
                    navigator: navigator.userAgent === "Polymer/Test",
                    storage: typeof Storage === "function" &&
                      sessionStorage instanceof Storage &&
                      sessionStorage.setItem("polymer-test", "stored") instanceof Promise,
                    deno: typeof Deno,
                    process: typeof process,
                    crypto: typeof crypto,
                  });
                })()"#,
            )
            .unwrap();
        let value = {
            deno_core::scope!(scope, runtime.js_runtime_mut());
            let value = deno_core::v8::Local::new(scope, value);
            value.to_rust_string_lossy(scope)
        };
        assert_eq!(
            value,
            r#"{"event":true,"url":"https://example.com/root/child","location":true,"channel":true,"timer":true,"fetch":true,"websocket":true,"navigator":true,"storage":true,"deno":"undefined","process":"undefined","crypto":"undefined"}"#
        );

        runtime
            .js_runtime_mut()
            .execute_script(
                "polymer:test/storage.js",
                r#"
                    globalThis.storageResult = undefined;
                    sessionStorage.setItem("polymer-test", "stored")
                      .then(() => sessionStorage.getItem("polymer-test"))
                      .then((value) => storageResult = value);
                "#,
            )
            .unwrap();
        runtime.run_up_to_duration(Duration::ZERO).await.unwrap();
        let value = runtime
            .js_runtime_mut()
            .execute_script("polymer:test/storage-result.js", "storageResult")
            .unwrap();
        let value = {
            deno_core::scope!(scope, runtime.js_runtime_mut());
            let value = deno_core::v8::Local::new(scope, value);
            value.to_rust_string_lossy(scope)
        };
        assert_eq!(value, "stored");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drives_fetch_and_timers() {
        let mut runtime = runtime();
        runtime
            .js_runtime_mut()
            .execute_script(
                "polymer:test/async.js",
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
            .execute_script("polymer:test/result.js", "JSON.stringify(result)")
            .unwrap();
        deno_core::scope!(scope, runtime.js_runtime_mut());
        let value = deno_core::v8::Local::new(scope, value);
        assert_eq!(value.to_rust_string_lossy(scope), r#"["ready","timer"]"#);
    }
}
