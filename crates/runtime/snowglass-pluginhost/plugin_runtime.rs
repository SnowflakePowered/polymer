use crate::filesystem::NandFactory;
use crate::{
    EmulatorLaunchArguments, EmulatorPluginInstance, EmulatorSetupInvocation, PluginRegistry,
};
use deno_core::{serde_v8, v8};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use thiserror::Error;
use tokio::sync::Notify;
use tokio::sync::Semaphore;

/// Scheduling policy for plugin workers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PluginRuntimeOptions {
    /// Maximum number of fresh isolates executing at the same time. Tokio's
    /// semaphore is fair, so excess calls are admitted in FIFO order.
    pub max_concurrent_calls: usize,
}

impl Default for PluginRuntimeOptions {
    fn default() -> Self {
        Self {
            max_concurrent_calls: 2,
        }
    }
}

/// Structured error returned to either DOM-facing adapter.
#[derive(Clone, Debug, Deserialize, Error, Serialize)]
#[error("{name}: {message}")]
#[serde(rename_all = "camelCase")]
pub struct PluginInvocationError {
    pub name: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack: Option<String>,
}

/// Result of a plugin call before application-owned resources are converted
/// into frontend-visible opaque handles.
pub struct PluginInvocationResult {
    pub value: Value,
    pub prepared_filesystem: Option<snowglass_vfs::PreparedFilesystemLaunch>,
}

impl PluginInvocationError {
    pub fn new(name: &str, code: &str, message: impl Into<String>) -> Self {
        Self {
            name: name.to_owned(),
            code: code.to_owned(),
            message: message.into(),
            stack: None,
        }
    }

    fn cancelled() -> Self {
        Self::new(
            "AbortError",
            "PLUGIN_CANCELLED",
            "plugin invocation was cancelled",
        )
    }

    fn worker(error: impl std::fmt::Display) -> Self {
        let message = error.to_string();
        Self {
            name: if message.contains("TypeError") {
                "TypeError".to_owned()
            } else {
                "Error".to_owned()
            },
            code: "PLUGIN_WORKER_FAILED".to_owned(),
            stack: Some(message.clone()),
            message,
        }
    }
}

struct InvocationControl {
    cancelled: AtomicBool,
    isolate: Mutex<Option<v8::IsolateHandle>>,
    cancelled_signal: Notify,
}

/// Single-use cancellation capability for one queued or executing invocation.
///
/// Adapters create this before scheduling work so a protocol cancellation can
/// reach the invocation even while it is waiting for a worker permit.
#[derive(Clone)]
pub struct PluginCancellationToken {
    control: Arc<InvocationControl>,
}

impl PluginCancellationToken {
    pub fn cancel(&self) {
        self.control.cancel();
    }
}

impl InvocationControl {
    fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            isolate: Mutex::new(None),
            cancelled_signal: Notify::new(),
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.cancelled_signal.notify_one();
        if let Some(isolate) = self.isolate.lock().unwrap().as_ref() {
            isolate.terminate_execution();
        }
    }

    fn install_isolate(&self, isolate: v8::IsolateHandle) -> bool {
        *self.isolate.lock().unwrap() = Some(isolate);
        self.cancelled.load(Ordering::Acquire)
    }
}

struct RuntimeState {
    registry: PluginRegistry,
    permits: Arc<Semaphore>,
    next_call: AtomicU64,
    active: Mutex<HashMap<u64, Arc<InvocationControl>>>,
    shutdown: AtomicBool,
}

/// Owns plugin scheduling and application-shutdown cancellation.
pub struct PluginRuntime {
    handle: PluginRuntimeHandle,
}

impl PluginRuntime {
    pub fn new(registry: PluginRegistry, options: PluginRuntimeOptions) -> Self {
        assert!(
            options.max_concurrent_calls > 0,
            "plugin runtime must allow at least one active call"
        );
        Self {
            handle: PluginRuntimeHandle {
                state: Arc::new(RuntimeState {
                    registry,
                    permits: Arc::new(Semaphore::new(options.max_concurrent_calls)),
                    next_call: AtomicU64::new(1),
                    active: Mutex::new(HashMap::new()),
                    shutdown: AtomicBool::new(false),
                }),
            },
        }
    }

    pub fn handle(&self) -> PluginRuntimeHandle {
        self.handle.clone()
    }
}

impl Drop for PluginRuntime {
    fn drop(&mut self) {
        self.handle.state.shutdown.store(true, Ordering::Release);
        self.handle.cancel_all();
        self.handle.state.permits.close();
    }
}

/// Cloneable entry point used by native coordinators and Deno op adapters.
#[derive(Clone)]
pub struct PluginRuntimeHandle {
    state: Arc<RuntimeState>,
}

impl PluginRuntimeHandle {
    pub fn registry(&self) -> &PluginRegistry {
        &self.state.registry
    }

    pub fn cancellation_token(&self) -> PluginCancellationToken {
        PluginCancellationToken {
            control: Arc::new(InvocationControl::new()),
        }
    }

    /// Invokes a callable export using JSON-compatible structured data.
    /// Property segments are resolved lazily in the fresh worker isolate.
    pub async fn invoke(
        &self,
        plugin_id: &str,
        export_path: &[String],
        arguments: Vec<Value>,
    ) -> Result<Value, PluginInvocationError> {
        let cancellation = self.cancellation_token();
        self.invoke_with_cancellation(plugin_id, export_path, arguments, cancellation)
            .await
    }

    pub async fn invoke_with_nand(
        &self,
        plugin_id: &str,
        export_path: &[String],
        arguments: Vec<Value>,
        nand: Option<NandFactory>,
    ) -> Result<Value, PluginInvocationError> {
        let cancellation = self.cancellation_token();
        let result = self
            .invoke_inner(plugin_id, export_path, arguments, nand, cancellation)
            .await?;
        Ok(result.value)
    }

    pub async fn invoke_with_nand_owned(
        &self,
        plugin_id: &str,
        export_path: &[String],
        arguments: Vec<Value>,
        nand: Option<NandFactory>,
    ) -> Result<PluginInvocationResult, PluginInvocationError> {
        let cancellation = self.cancellation_token();
        self.invoke_inner(plugin_id, export_path, arguments, nand, cancellation)
            .await
    }

    pub async fn invoke_with_cancellation(
        &self,
        plugin_id: &str,
        export_path: &[String],
        arguments: Vec<Value>,
        cancellation: PluginCancellationToken,
    ) -> Result<Value, PluginInvocationError> {
        let result = self
            .invoke_inner(plugin_id, export_path, arguments, None, cancellation)
            .await?;
        Ok(result.value)
    }

    async fn invoke_inner(
        &self,
        plugin_id: &str,
        export_path: &[String],
        arguments: Vec<Value>,
        nand: Option<NandFactory>,
        cancellation: PluginCancellationToken,
    ) -> Result<PluginInvocationResult, PluginInvocationError> {
        let plugin = self
            .state
            .registry
            .plugin::<dyn EmulatorPluginInstance>(plugin_id)
            .ok_or_else(|| {
                PluginInvocationError::new(
                    "NotFoundError",
                    "UNKNOWN_PLUGIN",
                    format!("unknown plugin '{plugin_id}'"),
                )
            })?;
        if export_path.is_empty() || export_path.iter().any(|segment| segment.is_empty()) {
            return Err(PluginInvocationError::new(
                "TypeError",
                "INVALID_EXPORT_PATH",
                "plugin export path must contain non-empty property names",
            ));
        }
        if matches!(export_path, [name] if name == "setupLaunch")
            || matches!(export_path, [default, name] if default == "default" && name == "setupLaunch")
        {
            let [arguments] = arguments.as_slice() else {
                return Err(PluginInvocationError::new(
                    "TypeError",
                    "INVALID_ARGUMENTS",
                    "setupLaunch expects exactly one argument object",
                ));
            };
            let console = arguments
                .get("console")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    PluginInvocationError::new(
                        "TypeError",
                        "INVALID_ARGUMENTS",
                        "setupLaunch arguments require a string console",
                    )
                })?;
            let assignments = serde_json::from_value(
                arguments.get("portAssignments").cloned().ok_or_else(|| {
                    PluginInvocationError::new(
                        "TypeError",
                        "INVALID_ARGUMENTS",
                        "setupLaunch arguments require portAssignments",
                    )
                })?,
            )
            .map_err(|error| {
                PluginInvocationError::new(
                    "TypeError",
                    "INVALID_ARGUMENTS",
                    format!("invalid setupLaunch portAssignments: {error}"),
                )
            })?;
            let mut configuration = plugin
                .materialize(console, assignments)
                .map_err(PluginInvocationError::worker)?;
            let settings = arguments
                .get("settings")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}))
                .as_object()
                .cloned()
                .ok_or_else(|| {
                    PluginInvocationError::new(
                        "TypeError",
                        "INVALID_ARGUMENTS",
                        "setupLaunch settings must be an object",
                    )
                })?;
            for (path, value) in settings {
                crate::configuration::set_value(&mut configuration, &path, value)
                    .map_err(PluginInvocationError::worker)?;
            }
            let mut plugin_arguments = arguments.clone();
            plugin_arguments
                .as_object_mut()
                .expect("validated setupLaunch arguments remain an object")
                .remove("settings");
            let mut result = self
                .setup_emulator_export_with_cancellation(
                    EmulatorSetupInvocation {
                        plugin,
                        arguments: plugin_arguments,
                        configuration,
                        nand,
                    },
                    cancellation,
                    matches!(export_path, [default, name] if default == "default" && name == "setupLaunch"),
                )
                .await?;
            let filesystem = result.filesystem.take();
            result.prepared_filesystem = None;
            let value = serde_json::to_value(result).map_err(PluginInvocationError::worker)?;
            return Ok(PluginInvocationResult {
                value,
                prepared_filesystem: filesystem,
            });
        }
        let entrypoint = plugin.entrypoint().to_string();
        let invocation_entrypoint = entrypoint.clone();
        let loader = plugin.module_loader().clone();
        let filesystem =
            filesystem_options(plugin.as_ref(), None).map_err(PluginInvocationError::worker)?;
        let path = export_path.to_vec();
        let value = self
            .run_with_control(cancellation.control, move |control| {
                run_worker(
                    entrypoint,
                    loader,
                    filesystem,
                    control,
                    move |runtime| async move {
                        let mut runtime = runtime;
                        invoke_export(
                            runtime.js_runtime_mut(),
                            &invocation_entrypoint,
                            &path,
                            arguments,
                        )
                        .await
                    },
                )
            })
            .await?;
        Ok(PluginInvocationResult {
            value,
            prepared_filesystem: None,
        })
    }

    /// Executes the emulator launch capability while keeping its mutable
    /// configuration proxy invocation-scoped inside the worker.
    pub async fn setup_emulator(
        &self,
        invocation: EmulatorSetupInvocation,
    ) -> Result<EmulatorLaunchArguments, PluginInvocationError> {
        let cancellation = self.cancellation_token();
        self.setup_emulator_with_cancellation(invocation, cancellation)
            .await
    }

    pub async fn setup_emulator_with_cancellation(
        &self,
        invocation: EmulatorSetupInvocation,
        cancellation: PluginCancellationToken,
    ) -> Result<EmulatorLaunchArguments, PluginInvocationError> {
        self.setup_emulator_export_with_cancellation(invocation, cancellation, false)
            .await
    }

    async fn setup_emulator_export_with_cancellation(
        &self,
        invocation: EmulatorSetupInvocation,
        cancellation: PluginCancellationToken,
        explicit_default: bool,
    ) -> Result<EmulatorLaunchArguments, PluginInvocationError> {
        let entrypoint = invocation.plugin.entrypoint().to_string();
        let loader = invocation.plugin.module_loader().clone();
        let filesystem = filesystem_options(invocation.plugin.as_ref(), invocation.nand.clone())
            .map_err(PluginInvocationError::worker)?;
        self.run_with_control(cancellation.control, move |control| {
            run_worker(
                entrypoint,
                loader,
                filesystem,
                control,
                move |mut runtime| async move {
                    crate::deno::execute_emulator(
                        runtime.js_runtime_mut(),
                        invocation,
                        explicit_default,
                    )
                    .await
                    .map_err(PluginInvocationError::worker)
                },
            )
        })
        .await
    }

    /// Cancels queued and executing calls. Executing V8 isolates receive a
    /// thread-safe termination request.
    pub fn cancel_all(&self) {
        for control in self.state.active.lock().unwrap().values() {
            control.cancel();
        }
    }

    #[cfg(test)]
    fn active_call_count(&self) -> usize {
        self.state.active.lock().unwrap().len()
    }

    #[cfg(test)]
    async fn run<T, F>(&self, worker: F) -> Result<T, PluginInvocationError>
    where
        T: Send + 'static,
        F: FnOnce(Arc<InvocationControl>) -> Result<T, PluginInvocationError> + Send + 'static,
    {
        self.run_with_control(Arc::new(InvocationControl::new()), worker)
            .await
    }

    async fn run_with_control<T, F>(
        &self,
        control: Arc<InvocationControl>,
        worker: F,
    ) -> Result<T, PluginInvocationError>
    where
        T: Send + 'static,
        F: FnOnce(Arc<InvocationControl>) -> Result<T, PluginInvocationError> + Send + 'static,
    {
        if self.state.shutdown.load(Ordering::Acquire) {
            return Err(PluginInvocationError::cancelled());
        }
        let call_id = self.state.next_call.fetch_add(1, Ordering::Relaxed);
        self.state
            .active
            .lock()
            .unwrap()
            .insert(call_id, control.clone());
        let mut guard = ActiveCallGuard {
            state: self.state.clone(),
            call_id,
            complete: false,
        };
        let permit = {
            let cancelled = control.cancelled_signal.notified();
            tokio::pin!(cancelled);
            if control.cancelled.load(Ordering::Acquire) {
                return Err(PluginInvocationError::cancelled());
            }
            tokio::select! {
                permit = self.state.permits.clone().acquire_owned() => {
                    permit.map_err(|_| PluginInvocationError::cancelled())?
                }
                () = &mut cancelled => return Err(PluginInvocationError::cancelled()),
            }
        };
        if control.cancelled.load(Ordering::Acquire) {
            return Err(PluginInvocationError::cancelled());
        }
        let worker_control = control.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            worker(worker_control)
        })
        .await
        .map_err(PluginInvocationError::worker)?;
        if control.cancelled.load(Ordering::Acquire) {
            return Err(PluginInvocationError::cancelled());
        }
        guard.complete = true;
        result
    }
}

struct ActiveCallGuard {
    state: Arc<RuntimeState>,
    call_id: u64,
    complete: bool,
}

impl Drop for ActiveCallGuard {
    fn drop(&mut self) {
        if let Some(control) = self.state.active.lock().unwrap().remove(&self.call_id)
            && !self.complete
        {
            control.cancel();
        }
    }
}

fn run_worker<T, F, Fut>(
    entrypoint: String,
    loader: crate::PluginModuleLoader,
    filesystem: crate::filesystem::FilesystemOptions,
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
        let main_module = deno_core::ModuleSpecifier::parse(&entrypoint)
            .map_err(PluginInvocationError::worker)?;
        let mut runtime = snowglass_deno::Runtime::new(snowglass_deno::RuntimeOptions {
            main_module,
            module_loader: Rc::new(loader),
            extensions: vec![
                crate::deno::extension(),
                crate::filesystem::extension(filesystem),
            ],
            user_agent: format!("SnowglassPluginHost/{}", env!("CARGO_PKG_VERSION")),
            storage_dir: None,
            inspect: false,
        });
        let isolate = runtime.js_runtime_mut().v8_isolate().thread_safe_handle();
        if control.install_isolate(isolate) {
            return Err(PluginInvocationError::cancelled());
        }
        operation(runtime).await
    })
}

fn filesystem_options(
    plugin: &dyn EmulatorPluginInstance,
    nand: Option<NandFactory>,
) -> Result<crate::filesystem::FilesystemOptions, crate::PackageError> {
    let mut options =
        crate::filesystem::FilesystemOptions::new(plugin.module_loader().root().to_owned());
    options.nand = nand;
    options.resources = plugin
        .resources()?
        .into_iter()
        .map(|resource| crate::filesystem::NamedResource {
            name: resource.name,
            path: resource.path,
            kind: match resource.format {
                crate::PluginResourceFormat::Directory => {
                    crate::filesystem::NamedResourceKind::Directory
                }
                crate::PluginResourceFormat::Asar => crate::filesystem::NamedResourceKind::Asar,
                crate::PluginResourceFormat::File => crate::filesystem::NamedResourceKind::File,
            },
            permissions: resource.permissions,
        })
        .collect();
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

use std::future::Future;

async fn invoke_export(
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

#[cfg(test)]
mod tests {
    use super::*;
    use snowglass_config::{
        ConfigurationToml,
        ast::{
            ConfigurationDefinition, DefaultDefinition, DomainDefinition, RealizedDomain,
            RealizedOption, Value as ConfigValue, ValueKind,
        },
    };
    use std::{collections::BTreeMap, convert::Infallible, sync::Arc, time::Duration};
    use tempfile::TempDir;
    use toml_spanner::Arena;

    fn package() -> (TempDir, PluginRegistry) {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("plugin.toml"),
            r#"
                [plugin]
                id = "worker-test"
                version = "1"
                class = "emulator"

                [[plugin.resources]]
                name = "runtime"
                source = "pkg:runtime"
                format = "directory"
                mode = "rx"

                [emulator]
                platforms = ["TEST"]
                main = "plugin.js"

                [emulator.configuration]
                definitions = ["config.toml"]
            "#,
        )
        .unwrap();
        std::fs::create_dir(root.path().join("runtime")).unwrap();
        std::fs::write(root.path().join("runtime/emulator.exe"), b"emulator").unwrap();
        std::fs::write(root.path().join("game.iso"), b"game").unwrap();
        std::fs::write(
            root.path().join("config.toml"),
            r#"
                [targets.settings]
                output = { format = "ini", path = "settings.ini" }

                [sections.general]
                title = "General"
                target = "settings"

                [[sections.general.options]]
                key = "enabled"
                title = "Enabled"
                type = "bool"
                default = false
                emit = { key = "Enabled" }
            "#,
        )
        .unwrap();
        std::fs::write(
            root.path().join("plugin.js"),
            r#"
                let isolateCounter = 0;
                export function named(value) {
                  isolateCounter++;
                  return { value, isolateCounter };
                }
                export async function asyncResult(value) {
                  await Promise.resolve();
                  return value * 2;
                }
                export function structured(value) {
                  return [
                    value.self === value,
                    value.bigint + 1n,
                    value.date.getTime(),
                    value.map.get("key"),
                    value.typed[1],
                    value,
                  ];
                }
                export function neverReturns() { while (true) {} }
                export function throwsError() {
                  throw new Error("worker-test-boom");
                }
                export const notCallable = 17;
                export function setupLaunch(args) {
                  args.configuration.options().general.enabled = true;
                  let command = "emulator.exe";
                  let argv = [];
                  if (args.virtualize) {
                    const firstRender = args.configuration.render();
                    const secondRender = args.configuration.render();
                    if (firstRender === secondRender || firstRender["settings.ini"] === secondRender["settings.ini"]) {
                      throw new Error("configuration.render() did not return fresh values");
                    }
                    args.filesystem.mountTree("/Emulator", args.resources.runtime, { mode: "rx" });
                    args.filesystem.mountFile("/Emulator/runtime-copy.exe", new URL("pkg:runtime/emulator.exe"), { mode: "r" });
                    for (const [name, bytes] of Object.entries(firstRender)) {
                      args.filesystem.mountFile(`/Emulator/Config/${name}`, bytes, { mode: "r" });
                    }
                    args.filesystem.mountHostFile("/Game/game.iso", args.launchPath, { mode: "r" });
                    args.filesystem.mountScratch("/User", { mode: "rwx", backing: "memory" });
                    args.filesystem.setWorkingDirectory("/Emulator");
                    args.filesystem.setProfile({ home: "/User", temp: "/User" });
                    command = args.filesystem.path("/Emulator/emulator.exe");
                    argv.push(args.filesystem.path("/Game/game.iso"));
                  }
                  return {
                    command,
                    argv,
                    configRoot: ".",
                    config: args.configuration,
                    hooks: ["vulkan"],
                    surfaces: [{ index: 0 }],
                  };
                }
                export default {
                  fallback(value) { return `fallback:${value}`; },
                  nonFunction: 23,
                  setupLaunch,
                };
            "#,
        )
        .unwrap();
        let registry = PluginRegistry::new();
        registry.load(root.path()).unwrap();
        (root, registry)
    }

    fn configuration() -> snowglass_config::ast::ConfigurationInstance {
        let source = std::fs::read_to_string(package().0.path().join("config.toml")).unwrap();
        let arena = Arena::new();
        let parsed =
            ConfigurationToml::try_from_tomls(&arena, [source.as_str()].into_iter()).unwrap();
        let definition = Arc::new(ConfigurationDefinition::from(parsed));
        let realized = definition
            .resolve_with(|_, option| -> Result<_, Infallible> {
                Ok(RealizedOption {
                    default: match &option.value.default {
                        DefaultDefinition::Literal(value) => value.clone(),
                        DefaultDefinition::None | DefaultDefinition::Resolver(_) => {
                            ConfigValue::Null
                        }
                    },
                    domain: match &option.value.kind {
                        ValueKind::Select {
                            domain: DomainDefinition::Static(_),
                        } => RealizedDomain::Static,
                        _ => RealizedDomain::None,
                    },
                })
            })
            .unwrap();
        realized.instantiate("test", BTreeMap::new()).unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolves_named_default_and_async_exports_in_fresh_isolates() {
        let (_root, registry) = package();
        let runtime = PluginRuntime::new(registry, PluginRuntimeOptions::default());
        let handle = runtime.handle();
        let first = handle
            .invoke(
                "worker-test",
                &["named".to_owned()],
                vec![serde_json::json!(7)],
            )
            .await
            .unwrap();
        let second = handle
            .invoke(
                "worker-test",
                &["named".to_owned()],
                vec![serde_json::json!(8)],
            )
            .await
            .unwrap();
        assert_eq!(
            first,
            serde_json::json!({ "value": 7, "isolateCounter": 1 })
        );
        assert_eq!(
            second,
            serde_json::json!({ "value": 8, "isolateCounter": 1 })
        );
        assert_eq!(
            handle
                .invoke(
                    "worker-test",
                    &["fallback".to_owned()],
                    vec![serde_json::json!("ok")],
                )
                .await
                .unwrap(),
            serde_json::json!("fallback:ok")
        );
        assert_eq!(
            handle
                .invoke(
                    "worker-test",
                    &["asyncResult".to_owned()],
                    vec![serde_json::json!(6)],
                )
                .await
                .unwrap(),
            serde_json::json!(12)
        );
        assert_eq!(handle.active_call_count(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_unknown_missing_non_callable_and_thrown_exports_with_structured_errors() {
        let (_root, registry) = package();
        let runtime = PluginRuntime::new(registry, PluginRuntimeOptions::default());
        let handle = runtime.handle();

        let unknown = handle
            .invoke("absent", &["run".to_owned()], Vec::new())
            .await
            .unwrap_err();
        assert_eq!(unknown.name, "NotFoundError");
        assert_eq!(unknown.code, "UNKNOWN_PLUGIN");

        for path in [
            vec!["missing".to_owned()],
            vec!["notCallable".to_owned()],
            vec!["default".to_owned(), "nonFunction".to_owned()],
        ] {
            let error = handle
                .invoke("worker-test", &path, Vec::new())
                .await
                .unwrap_err();
            assert_eq!(error.name, "TypeError");
            assert_eq!(error.code, "PLUGIN_WORKER_FAILED");
            assert!(error.message.contains("is not a function"));
            assert!(
                error
                    .stack
                    .as_deref()
                    .is_some_and(|stack| !stack.is_empty())
            );
        }

        let thrown = handle
            .invoke("worker-test", &["throwsError".to_owned()], Vec::new())
            .await
            .unwrap_err();
        assert_eq!(thrown.name, "Error");
        assert_eq!(thrown.code, "PLUGIN_WORKER_FAILED");
        assert!(thrown.message.contains("worker-test-boom"));
        assert!(
            thrown
                .stack
                .as_deref()
                .is_some_and(|stack| stack.contains("throwsError"))
        );
        assert_eq!(handle.active_call_count(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn admits_two_workers_and_queues_the_third_in_fifo_order() {
        use std::sync::atomic::AtomicBool;

        let (_root, registry) = package();
        let runtime = PluginRuntime::new(
            registry,
            PluginRuntimeOptions {
                max_concurrent_calls: 2,
            },
        );
        let handle = runtime.handle();
        let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
        let gates = (0..3)
            .map(|_| Arc::new(AtomicBool::new(false)))
            .collect::<Vec<_>>();
        let spawn_call = |id: usize| {
            let handle = handle.clone();
            let started = started.clone();
            let gate = gates[id].clone();
            tokio::spawn(async move {
                handle
                    .run(move |_| {
                        started.send(id).unwrap();
                        while !gate.load(Ordering::Acquire) {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Ok(id)
                    })
                    .await
            })
        };

        let first = spawn_call(0);
        assert_eq!(starts.recv().await, Some(0));
        let second = spawn_call(1);
        assert_eq!(starts.recv().await, Some(1));
        let third = spawn_call(2);
        assert!(
            tokio::time::timeout(Duration::from_millis(25), starts.recv())
                .await
                .is_err(),
            "the third call entered without waiting for a worker slot"
        );

        gates[0].store(true, Ordering::Release);
        assert_eq!(starts.recv().await, Some(2));
        gates[1].store(true, Ordering::Release);
        gates[2].store(true, Ordering::Release);
        assert_eq!(first.await.unwrap().unwrap(), 0);
        assert_eq!(second.await.unwrap().unwrap(), 1);
        assert_eq!(third.await.unwrap().unwrap(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn realizes_configuration_before_discarding_the_worker() {
        let (root, registry) = package();
        let plugin = registry
            .plugin::<dyn EmulatorPluginInstance>("worker-test")
            .unwrap();
        let runtime = PluginRuntime::new(registry, PluginRuntimeOptions::default());
        let result = runtime
            .handle()
            .setup_emulator(EmulatorSetupInvocation {
                plugin,
                arguments: serde_json::json!({
                    "launchPath": root.path().join("game.iso"),
                    "virtualize": true
                }),
                configuration: configuration(),
                nand: None,
            })
            .await
            .unwrap();
        assert!(
            std::str::from_utf8(&result.config["settings.ini"])
                .unwrap()
                .contains("Enabled=true")
        );
        let filesystem = result
            .filesystem
            .as_ref()
            .expect("plugin populated a VFS plan");
        assert_eq!(filesystem.filesystem().compiled().bindings().len(), 5);
        assert!(result.command.starts_with(r"\\Snowglass\"));
        assert!(result.argv[0].ends_with(r"\Game\game.iso"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn facade_setup_launch_materializes_its_invocation_configuration() {
        let (_root, registry) = package();
        let runtime = PluginRuntime::new(registry, PluginRuntimeOptions::default());
        let result = runtime
            .handle()
            .invoke(
                "worker-test",
                &["setupLaunch".to_owned()],
                vec![serde_json::json!({
                    "launchPath": "game",
                    "console": "TEST",
                    "portAssignments": {},
                })],
            )
            .await
            .unwrap();
        assert_eq!(result["command"], "emulator.exe");
        assert!(result["config"]["settings.ini"].is_array());

        let explicit_default = runtime
            .handle()
            .invoke(
                "worker-test",
                &["default".to_owned(), "setupLaunch".to_owned()],
                vec![serde_json::json!({
                    "launchPath": "game",
                    "console": "TEST",
                    "portAssignments": {},
                })],
            )
            .await
            .unwrap();
        assert_eq!(explicit_default["command"], "emulator.exe");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prepared_filesystems_are_returned_to_the_application_owner() {
        let (root, registry) = package();
        let runtime = PluginRuntime::new(registry, PluginRuntimeOptions::default());
        let handle = runtime.handle();
        let result = handle
            .invoke_with_nand_owned(
                "worker-test",
                &["setupLaunch".to_owned()],
                vec![serde_json::json!({
                    "launchPath": root.path().join("game.iso"),
                    "console": "TEST",
                    "portAssignments": {},
                    "virtualize": true,
                })],
                None,
            )
            .await
            .unwrap();
        assert!(result.value["preparedFilesystem"].is_null());
        let filesystem = result.prepared_filesystem.unwrap();
        assert_eq!(filesystem.filesystem().compiled().bindings().len(), 5);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_terminates_executing_v8_code() {
        let (_root, registry) = package();
        let runtime = PluginRuntime::new(registry, PluginRuntimeOptions::default());
        let handle = runtime.handle();
        let cancellation = handle.clone();
        let path = ["neverReturns".to_owned()];
        let call = handle.invoke("worker-test", &path, vec![]);
        tokio::pin!(call);
        tokio::select! {
            result = &mut call => panic!("worker finished before cancellation: {result:?}"),
            () = tokio::time::sleep(Duration::from_millis(25)) => cancellation.cancel_all(),
        }
        let error = tokio::time::timeout(Duration::from_secs(2), call)
            .await
            .expect("termination did not stop V8")
            .unwrap_err();
        assert_eq!(error.code, "PLUGIN_CANCELLED");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invocation_cancellation_does_not_abort_an_unrelated_call() {
        let (_root, registry) = package();
        let runtime = PluginRuntime::new(
            registry,
            PluginRuntimeOptions {
                max_concurrent_calls: 1,
            },
        );
        let handle = runtime.handle();
        let first_token = handle.cancellation_token();
        let first_cancel = first_token.clone();
        let first_handle = handle.clone();
        let first = tokio::spawn(async move {
            first_handle
                .invoke_with_cancellation(
                    "worker-test",
                    &["neverReturns".to_owned()],
                    Vec::new(),
                    first_token,
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(25)).await;

        let second_handle = handle.clone();
        let second = tokio::spawn(async move {
            second_handle
                .invoke(
                    "worker-test",
                    &["asyncResult".to_owned()],
                    vec![serde_json::json!(9)],
                )
                .await
        });
        first_cancel.cancel();

        assert!(first.await.unwrap().is_err());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), second)
                .await
                .expect("the unrelated call stayed queued")
                .unwrap()
                .unwrap(),
            serde_json::json!(18)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queued_invocation_cancels_without_waiting_for_a_worker_slot() {
        let (_root, registry) = package();
        let runtime = PluginRuntime::new(
            registry,
            PluginRuntimeOptions {
                max_concurrent_calls: 1,
            },
        );
        let handle = runtime.handle();
        let gate = Arc::new(AtomicBool::new(false));
        let worker_gate = gate.clone();
        let (started, started_rx) = std::sync::mpsc::channel();
        let first_handle = handle.clone();
        let first = tokio::spawn(async move {
            first_handle
                .run(move |_| {
                    started.send(()).unwrap();
                    while !worker_gate.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Ok(())
                })
                .await
        });
        tokio::task::yield_now().await;
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let queued_token = handle.cancellation_token();
        let queued_cancel = queued_token.clone();
        let queued_handle = handle.clone();
        let queued = tokio::spawn(async move {
            queued_handle
                .invoke_with_cancellation(
                    "worker-test",
                    &["asyncResult".to_owned()],
                    vec![serde_json::json!(3)],
                    queued_token,
                )
                .await
        });
        tokio::task::yield_now().await;
        queued_cancel.cancel();
        let error = tokio::time::timeout(Duration::from_millis(250), queued)
            .await
            .expect("queued cancellation waited for a worker slot")
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, "PLUGIN_CANCELLED");

        gate.store(true, Ordering::Release);
        first.await.unwrap().unwrap();
    }
}
