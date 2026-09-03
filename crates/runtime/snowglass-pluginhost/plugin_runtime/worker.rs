use super::{InvocationControl, PluginInvocationError, PluginLifecycle};
use crate::{EmulatorPluginInstance, filesystem::StorageFactories};
use deno_core::{serde_v8, v8};
use serde::Deserialize;
use serde_json::Value;
use std::{future::Future, rc::Rc, sync::Arc};
pub(super) fn run_worker<T, F, Fut>(
    entrypoint: String,
    loader: crate::PluginModuleLoader,
    filesystem: crate::filesystem::FilesystemOptions,
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
        let mut runtime =
            create_worker_runtime(&entrypoint, loader, filesystem, lifecycle.storage.clone())?;
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
    loader: crate::PluginModuleLoader,
    filesystem: crate::filesystem::FilesystemOptions,
    storage: Arc<crate::deno::plugin_storage::PluginStorage>,
) -> Result<snowglass_deno::Runtime, PluginInvocationError> {
    let main_module =
        deno_core::ModuleSpecifier::parse(entrypoint).map_err(PluginInvocationError::worker)?;
    Ok(snowglass_deno::Runtime::new(
        snowglass_deno::RuntimeOptions {
            main_module,
            module_loader: Rc::new(loader),
            extensions: vec![
                crate::deno::extension(),
                crate::filesystem::extension(filesystem),
                crate::deno::plugin_storage::extension(storage),
            ],
            user_agent: format!("SnowglassPluginHost/{}", env!("CARGO_PKG_VERSION")),
            local_storage: None,
            inspect: false,
        },
    ))
}

pub(super) fn filesystem_options(
    plugin: &dyn EmulatorPluginInstance,
    storage: StorageFactories,
    quick_resume: Option<crate::QuickResumeData>,
) -> Result<crate::filesystem::FilesystemOptions, crate::PackageError> {
    let mut options =
        crate::filesystem::FilesystemOptions::new(plugin.module_loader().root().to_owned());
    options.storage = storage;
    options.quick_resume = quick_resume;
    options.resources = plugin.resources()?;
    if !plugin.caches().is_empty() {
        let plugin_cache_root = local_cache_root()?.join(".snowglass").join(plugin.id());
        options.caches = plugin
            .caches()
            .iter()
            .map(|cache| crate::filesystem::NamedCache {
                key: cache.key.clone(),
                path: plugin_cache_root.join(&cache.key),
            })
            .collect();
    }
    Ok(options)
}

fn local_cache_root() -> Result<std::path::PathBuf, crate::PackageError> {
    #[cfg(windows)]
    let root = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from);

    #[cfg(target_os = "linux")]
    let root = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .map(|home| home.join(".cache"))
        });

    #[cfg(not(any(windows, target_os = "linux")))]
    let root = None;

    root.filter(|path| path.is_absolute())
        .ok_or(crate::PackageError::CacheStorageUnavailable)
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
