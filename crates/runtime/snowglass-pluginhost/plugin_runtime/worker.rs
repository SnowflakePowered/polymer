use super::{InvocationControl, PluginInvocationError, PluginLifecycle};
use crate::{PluginInstance, filesystem::StorageFactories};
use deno_core::{serde_v8, v8};
use serde::Deserialize;
use serde_json::Value;
use std::{future::Future, rc::Rc, sync::Arc};

const MAX_SCRAPE_RESULTS: usize = 4096;

pub(super) struct WorkerOptions {
    loader: crate::PluginModuleLoader,
    filesystem: crate::filesystem::FilesystemOptions,
    cache: crate::cache::CacheOptions,
    database: crate::database::DatabaseOptions,
}

impl WorkerOptions {
    pub(super) fn for_plugin(
        plugin: &dyn PluginInstance,
        directories: &snowglass_library::Directories,
        storage: StorageFactories,
        quick_resume: Option<crate::QuickResumeData>,
    ) -> Result<Self, crate::PackageError> {
        let mut filesystem =
            crate::filesystem::FilesystemOptions::new(plugin.module_loader().root().to_owned());
        filesystem.scratch_root = Some(directories.plugin_scratch(plugin.id()));
        filesystem.storage = storage;
        filesystem.quick_resume = quick_resume;
        filesystem.resources = plugin.resources()?;
        let cache = crate::cache::CacheOptions::for_plugin(directories, plugin);
        filesystem.caches = cache.filesystem_caches();
        Ok(Self {
            loader: plugin.module_loader().with_directories(directories),
            filesystem,
            cache,
            database: crate::database::DatabaseOptions::for_plugin(directories, plugin.id()),
        })
    }
}

pub(super) fn run_worker<T, F, Fut>(
    entrypoint: String,
    options: WorkerOptions,
    lifecycle: Arc<PluginLifecycle>,
    control: Arc<InvocationControl>,
    operation: F,
) -> Result<T, PluginInvocationError>
where
    F: FnOnce(snowglass_deno::Runtime) -> Fut,
    Fut: Future<Output = Result<T, PluginInvocationError>>,
{
    let tokio = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(PluginInvocationError::worker)?;
    tokio.block_on(async move {
        let mut runtime = create_worker_runtime(&entrypoint, options, lifecycle.storage.clone())?;
        let isolate = runtime.js_runtime_mut().v8_isolate().thread_safe_handle();
        if control.install_isolate(isolate) {
            return Err(PluginInvocationError::cancelled());
        }
        lifecycle.load(&mut runtime, &entrypoint).await?;
        operation(runtime).await
    })
}

pub(super) fn create_worker_runtime(
    entrypoint: &str,
    options: WorkerOptions,
    storage: Arc<crate::deno::plugin_storage::PluginStorage>,
) -> Result<snowglass_deno::Runtime, PluginInvocationError> {
    let WorkerOptions {
        loader,
        filesystem,
        cache,
        database,
    } = options;
    let main_module =
        deno_core::ModuleSpecifier::parse(entrypoint).map_err(PluginInvocationError::worker)?;
    Ok(snowglass_deno::Runtime::new(
        snowglass_deno::RuntimeOptions {
            main_module,
            module_loader: Rc::new(loader),
            extensions: vec![
                crate::deno::extension(),
                crate::cache::extension(cache),
                crate::database::extension(database),
                crate::filesystem::extension(filesystem),
                crate::deno::plugin_storage::extension(storage),
            ],
            user_agent: format!("SnowglassPluginHost/{}", env!("CARGO_PKG_VERSION")),
            local_storage: None,
            inspect: false,
        },
    ))
}

pub(super) async fn invoke_scrape_generator(
    runtime: &mut deno_core::JsRuntime,
    entrypoint: &str,
    input: &crate::ScrapeInput,
) -> Result<Vec<crate::ScrapeResult>, PluginInvocationError> {
    let entrypoint = serde_json::to_string(entrypoint).map_err(PluginInvocationError::worker)?;
    let input = serde_json::to_string(input).map_err(PluginInvocationError::worker)?;
    let initialize = format!(
        r#"(async () => {{
          const namespace = await import({entrypoint});
          const callback = namespace.onScrape ?? namespace.default?.onScrape;
          if (typeof callback !== "function") {{
            throw new TypeError("plugin export 'onScrape' is not a function");
          }}
          const generator = Reflect.apply(callback, namespace, [{input}]);
          if (Object.prototype.toString.call(generator) !== "[object AsyncGenerator]" ||
              generator[Symbol.asyncIterator]?.() !== generator) {{
            throw new TypeError("onScrape must return an async generator");
          }}
          globalThis.__snowglassScrapeGenerator = generator;
        }})()"#
    );
    let promise = runtime
        .execute_script("snowglass:pluginhost/scrape-initialize.js", initialize)
        .map_err(PluginInvocationError::worker)?;
    #[allow(deprecated)]
    runtime
        .resolve_value(promise)
        .await
        .map_err(PluginInvocationError::worker)?;

    #[derive(serde::Deserialize)]
    struct GeneratorStep {
        done: bool,
        #[serde(default)]
        value: Option<crate::ScrapeResult>,
    }

    let mut results = Vec::new();
    loop {
        let script_name = format!("snowglass:pluginhost/scrape-next-{}.js", results.len());
        let promise = runtime
            .execute_script(
                script_name,
                r#"globalThis.__snowglassScrapeGenerator.next().then((step) => {
                  if (step.done) return { done: true };
                  const value = step.value;
                  if (value === null || typeof value !== "object" || Array.isArray(value)) {
                    throw new TypeError("onScrape must yield { key, value, source } objects");
                  }
                  return { done: false, value };
                })"#,
            )
            .map_err(PluginInvocationError::worker)?;
        #[allow(deprecated)]
        let resolved = runtime
            .resolve_value(promise)
            .await
            .map_err(PluginInvocationError::worker)?;
        let step = {
            deno_core::scope!(scope, runtime);
            let value = v8::Local::new(scope, resolved);
            serde_v8::from_v8::<GeneratorStep>(scope, value)
                .map_err(PluginInvocationError::worker)?
        };
        if step.done {
            break;
        }
        if results.len() == MAX_SCRAPE_RESULTS {
            return Err(PluginInvocationError::new(
                "RangeError",
                "SCRAPE_RESULT_LIMIT",
                format!("onScrape yielded more than {MAX_SCRAPE_RESULTS} results"),
            ));
        }
        results.push(step.value.ok_or_else(|| {
            PluginInvocationError::new(
                "TypeError",
                "INVALID_SCRAPE_RESULT",
                "onScrape yielded no result value",
            )
        })?);
    }
    Ok(results)
}

pub(super) async fn invoke_export(
    runtime: &mut deno_core::JsRuntime,
    entrypoint: &str,
    export_path: &[String],
    arguments: Vec<Value>,
) -> Result<Value, PluginInvocationError> {
    let entrypoint = serde_json::to_string(entrypoint).map_err(PluginInvocationError::worker)?;
    let path = serde_json::to_string(export_path).map_err(PluginInvocationError::worker)?;
    let arguments = serde_json::to_string(&arguments).map_err(PluginInvocationError::worker)?;
    let script = format!(
        r#"(async () => {{
          const namespace = await import({entrypoint});
          const path = {path};
          let receiver;
          let value;
          if (path[0] === "default") {{
            value = namespace.default;
            receiver = namespace;
            for (const segment of path.slice(1)) {{ receiver = value; value = value?.[segment]; }}
          }} else {{
            value = namespace[path[0]];
            receiver = namespace;
            if (value === undefined) {{ receiver = namespace.default; value = receiver?.[path[0]]; }}
            for (const segment of path.slice(1)) {{ receiver = value; value = value?.[segment]; }}
          }}
          if (typeof value !== "function") {{
            throw new TypeError(`plugin export '${{path.join(".")}}' is not a function`);
          }}
          return await Reflect.apply(value, receiver, {arguments});
        }})()"#
    );
    let promise = runtime
        .execute_script("snowglass:pluginhost/invoke.js", script)
        .map_err(PluginInvocationError::worker)?;
    #[allow(deprecated)]
    let resolved = runtime
        .resolve_value(promise)
        .await
        .map_err(PluginInvocationError::worker)?;
    deno_core::scope!(scope, runtime);
    let value = v8::Local::new(scope, resolved);
    serde_v8::from_v8(scope, value).map_err(PluginInvocationError::worker)
}

pub(super) async fn invoke_optional_export(
    runtime: &mut deno_core::JsRuntime,
    entrypoint: &str,
    name: &str,
    arguments: Vec<Value>,
) -> Result<Option<Value>, PluginInvocationError> {
    let entrypoint = serde_json::to_string(entrypoint).map_err(PluginInvocationError::worker)?;
    let name = serde_json::to_string(name).map_err(PluginInvocationError::worker)?;
    let arguments = serde_json::to_string(&arguments).map_err(PluginInvocationError::worker)?;
    let script = format!(
        r#"(async () => {{
          const namespace = await import({entrypoint});
          const name = {name};
          let receiver = namespace;
          let value = namespace[name];
          if (value === undefined) {{ receiver = namespace.default; value = receiver?.[name]; }}
          if (value === undefined) return {{ found: false, value: null }};
          if (typeof value !== "function") {{
            throw new TypeError(`plugin export '${{name}}' is not a function`);
          }}
          const result = await Reflect.apply(value, receiver, {arguments});
          return {{ found: true, value: result === undefined ? null : result }};
        }})()"#
    );
    let promise = runtime
        .execute_script("snowglass:pluginhost/invoke-optional.js", script)
        .map_err(PluginInvocationError::worker)?;
    #[allow(deprecated)]
    let resolved = runtime
        .resolve_value(promise)
        .await
        .map_err(PluginInvocationError::worker)?;
    deno_core::scope!(scope, runtime);
    let value = v8::Local::new(scope, resolved);
    let value: OptionalExportResult =
        serde_v8::from_v8(scope, value).map_err(PluginInvocationError::worker)?;
    Ok(value.found.then_some(value.value))
}

#[derive(Deserialize)]
struct OptionalExportResult {
    found: bool,
    value: Value,
}
