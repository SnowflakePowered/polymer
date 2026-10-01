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

pub mod digest;
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
    /// Hosts whose certificates Fetch does not verify.
    pub insecure_hosts: Vec<String>,
}

/// A `deno_core` isolate with a curated browser-like global scope.
pub struct Runtime {
    js_runtime: JsRuntime,
}

impl Runtime {
    pub fn network_permissions(
        hosts: &[String],
    ) -> Result<PermissionsContainer, deno_permissions::PermissionsFromOptionsError> {
        let parser = Arc::new(RuntimePermissionDescriptorParser::new(
            sys_traits::impls::RealSys,
        ));
        let permissions = deno_permissions::Permissions::from_options(
            parser.as_ref(),
            &deno_permissions::PermissionsOptions {
                allow_net: (!hosts.is_empty()).then(|| hosts.to_vec()),
                prompt: false,
                ..Default::default()
            },
        )?;
        Ok(PermissionsContainer::new(parser, permissions))
    }

    /// Check `url` against the network permissions set by [`Runtime::restrict_network`],
    /// for host APIs that make requests on a plugin's behalf.
    pub fn check_network(
        state: &mut deno_core::OpState,
        url: &str,
        api: &str,
    ) -> Result<(), deno_error::JsErrorBox> {
        let url = deno_core::url::Url::parse(url).map_err(deno_error::JsErrorBox::from_err)?;
        state
            .borrow_mut::<PermissionsContainer>()
            .check_net_url(&url, api)
            .map_err(|error| deno_error::JsErrorBox::new("NotCapable", error.to_string()))
    }

    pub fn restrict_network(
        &mut self,
        hosts: &[String],
    ) -> Result<(), deno_permissions::PermissionsFromOptionsError> {
        self.js_runtime
            .op_state()
            .borrow_mut()
            .put(Self::network_permissions(hosts)?);
        Ok(())
    }

    pub fn new(mut options: RuntimeOptions) -> Self {
        // A workspace embedder can unify rustls dependencies that select both
        // built-in providers. Pick the provider used by Deno's networking
        // extensions before they lazily construct a TLS client.
        let _ = deno_tls::rustls::crypto::aws_lc_rs::default_provider().install_default();
        let location = options.main_module.clone();
        let user_agent = options.user_agent;
        let mut extensions = vec![
            digest::DigestHandler::extension(),
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
                // Deno ignores certificate errors for every host when the list is empty.
                unsafely_ignore_certificate_errors: (!options.insecure_hosts.is_empty())
                    .then(|| std::mem::take(&mut options.insecure_hosts)),
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
