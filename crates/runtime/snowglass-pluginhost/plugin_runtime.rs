use crate::filesystem::StorageFactories;
use crate::{
    EmulatorLaunchArguments, EmulatorPluginInstance, EmulatorSetupInvocation, PluginRegistry,
};
use deno_core::{serde_v8, v8};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
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
    pub controller_session: Option<crate::ControllerSession>,
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

    pub(crate) fn worker(error: impl std::fmt::Display) -> Self {
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
        if let Some(isolate) = self.isolate.lock().as_ref() {
            isolate.terminate_execution();
        }
    }

    fn install_isolate(&self, isolate: v8::IsolateHandle) -> bool {
        *self.isolate.lock() = Some(isolate);
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

    pub async fn invoke_with_storage(
        &self,
        plugin_id: &str,
        export_path: &[String],
        arguments: Vec<Value>,
        storage: StorageFactories,
    ) -> Result<Value, PluginInvocationError> {
        let cancellation = self.cancellation_token();
        let result = self
            .invoke_inner(plugin_id, export_path, arguments, storage, cancellation)
            .await?;
        Ok(result.value)
    }

    pub async fn invoke_with_storage_owned(
        &self,
        plugin_id: &str,
        export_path: &[String],
        arguments: Vec<Value>,
        storage: StorageFactories,
    ) -> Result<PluginInvocationResult, PluginInvocationError> {
        let cancellation = self.cancellation_token();
        self.invoke_inner(plugin_id, export_path, arguments, storage, cancellation)
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
            .invoke_inner(
                plugin_id,
                export_path,
                arguments,
                BTreeMap::new(),
                cancellation,
            )
            .await?;
        Ok(result.value)
    }

    async fn invoke_inner(
        &self,
        plugin_id: &str,
        export_path: &[String],
        arguments: Vec<Value>,
        storage: StorageFactories,
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
            let controller_changes = self
                .has_controller_changes(plugin.clone(), cancellation.control.clone())
                .await?;
            let expanded = controller_changes
                .then(|| {
                    crate::controller::ControllerSetup::expand(plugin.as_ref(), console, arguments)
                })
                .transpose()?;
            let plugin_arguments = expanded
                .as_ref()
                .map_or_else(|| arguments.clone(), |expanded| expanded.arguments.clone());
            let assignments = if let Some(expanded) = &expanded {
                expanded.assignments.clone()
            } else {
                serde_json::from_value(arguments.get("portAssignments").cloned().ok_or_else(
                    || {
                        PluginInvocationError::new(
                            "TypeError",
                            "INVALID_ARGUMENTS",
                            "setupLaunch arguments require portAssignments",
                        )
                    },
                )?)
                .map_err(|error| {
                    PluginInvocationError::new(
                        "TypeError",
                        "INVALID_ARGUMENTS",
                        format!("invalid setupLaunch portAssignments: {error}"),
                    )
                })?
            };
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
            let mut plugin_arguments = plugin_arguments;
            {
                let object = plugin_arguments
                    .as_object_mut()
                    .expect("validated setupLaunch arguments remain an object");
                object.remove("settings");
                object.remove("storage");
            }
            let invocation = EmulatorSetupInvocation {
                plugin,
                arguments: plugin_arguments,
                configuration,
                storage,
            };
            let (mut result, session) = if let Some(expanded) = expanded {
                let (launch, session) = self
                    .setup_controller_session_with_cancellation(
                        invocation,
                        cancellation,
                        matches!(export_path, [default, name] if default == "default" && name == "setupLaunch"),
                        expanded.connected,
                        expanded.gamepad_mapping,
                    )
                    .await?;
                (launch, Some(session))
            } else {
                (
                    self.setup_emulator_export_with_cancellation(
                        invocation,
                        cancellation,
                        matches!(export_path, [default, name] if default == "default" && name == "setupLaunch"),
                    )
                    .await?,
                    None,
                )
            };
            let filesystem = result.filesystem.take();
            let retained_session = result.session.take().or(session);
            result.prepared_filesystem = None;
            let value = serde_json::to_value(result).map_err(PluginInvocationError::worker)?;
            return Ok(PluginInvocationResult {
                value,
                prepared_filesystem: filesystem,
                controller_session: retained_session,
            });
        }
        let entrypoint = plugin.entrypoint().to_string();
        let invocation_entrypoint = entrypoint.clone();
        let loader = plugin.module_loader().clone();
        let filesystem = filesystem_options(plugin.as_ref(), BTreeMap::new())
            .map_err(PluginInvocationError::worker)?;
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
            controller_session: None,
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
        let filesystem = filesystem_options(invocation.plugin.as_ref(), invocation.storage.clone())
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

    async fn has_controller_changes(
        &self,
        plugin: Arc<dyn EmulatorPluginInstance>,
        control: Arc<InvocationControl>,
    ) -> Result<bool, PluginInvocationError> {
        let entrypoint = plugin.entrypoint().to_string();
        let invocation_entrypoint = entrypoint.clone();
        let loader = plugin.module_loader().clone();
        let filesystem = filesystem_options(plugin.as_ref(), BTreeMap::new())
            .map_err(PluginInvocationError::worker)?;
        self.run_with_control(control, move |control| {
            run_worker(
                entrypoint,
                loader,
                filesystem,
                control,
                move |mut runtime| async move {
                    has_callable_controller_export(runtime.js_runtime_mut(), &invocation_entrypoint)
                        .await
                },
            )
        })
        .await
    }

    async fn setup_controller_session_with_cancellation(
        &self,
        invocation: EmulatorSetupInvocation,
        cancellation: PluginCancellationToken,
        explicit_default: bool,
        connected: Vec<bool>,
        gamepad_mapping: Value,
    ) -> Result<(EmulatorLaunchArguments, crate::ControllerSession), PluginInvocationError> {
        let filesystem = filesystem_options(invocation.plugin.as_ref(), invocation.storage.clone())
            .map_err(PluginInvocationError::worker)?;
        self.run_with_control(cancellation.control, move |control| {
            start_controller_session_worker(
                invocation,
                explicit_default,
                filesystem,
                control,
                connected,
                gamepad_mapping,
            )
        })
        .await
    }

    /// Cancels queued and executing calls. Executing V8 isolates receive a
    /// thread-safe termination request.
    pub fn cancel_all(&self) {
        for control in self.state.active.lock().values() {
            control.cancel();
        }
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
        self.state.active.lock().insert(call_id, control.clone());
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
        if let Some(control) = self.state.active.lock().remove(&self.call_id)
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
        let mut runtime = create_worker_runtime(&entrypoint, loader, filesystem)?;
        let isolate = runtime.js_runtime_mut().v8_isolate().thread_safe_handle();
        if control.install_isolate(isolate) {
            return Err(PluginInvocationError::cancelled());
        }
        operation(runtime).await
    })
}

fn create_worker_runtime(
    entrypoint: &str,
    loader: crate::PluginModuleLoader,
    filesystem: crate::filesystem::FilesystemOptions,
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
            ],
            user_agent: format!("SnowglassPluginHost/{}", env!("CARGO_PKG_VERSION")),
            storage_dir: None,
            inspect: false,
        },
    ))
}

fn start_controller_session_worker(
    invocation: EmulatorSetupInvocation,
    explicit_default: bool,
    filesystem: crate::filesystem::FilesystemOptions,
    control: Arc<InvocationControl>,
    connected: Vec<bool>,
    gamepad_mapping: Value,
) -> Result<(EmulatorLaunchArguments, crate::ControllerSession), PluginInvocationError> {
    let entrypoint = invocation.plugin.entrypoint().to_string();
    let loader = invocation.plugin.module_loader().clone();
    let (commands, command_receiver) = std::sync::mpsc::channel();
    let errors = Arc::new(Mutex::new(Vec::new()));
    let worker_errors = errors.clone();
    let (started, startup) = std::sync::mpsc::sync_channel(1);
    thread::Builder::new()
        .name(format!(
            "snowglass-plugin-session-{}",
            invocation.plugin.id()
        ))
        .spawn(move || {
            let result = controller_session_worker(
                entrypoint,
                loader,
                filesystem,
                control,
                invocation,
                explicit_default,
                connected,
                gamepad_mapping,
                command_receiver,
                worker_errors,
                started,
            );
            if let Err(error) = result {
                tracing::error!("snowglass: controller plugin session failed: {error}");
            }
        })
        .map_err(PluginInvocationError::worker)?;
    let launch = startup.recv().map_err(|_| {
        PluginInvocationError::worker("controller plugin session failed to start")
    })??;
    Ok((launch, crate::ControllerSession::new(commands, errors)))
}

#[expect(clippy::too_many_arguments)]
fn controller_session_worker(
    entrypoint: String,
    loader: crate::PluginModuleLoader,
    filesystem: crate::filesystem::FilesystemOptions,
    control: Arc<InvocationControl>,
    invocation: EmulatorSetupInvocation,
    explicit_default: bool,
    connected: Vec<bool>,
    gamepad_mapping: Value,
    commands: std::sync::mpsc::Receiver<crate::controller::ControllerSessionCommand>,
    errors: Arc<Mutex<Vec<String>>>,
    started: std::sync::mpsc::SyncSender<Result<EmulatorLaunchArguments, PluginInvocationError>>,
) -> Result<(), PluginInvocationError> {
    let tokio = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(PluginInvocationError::worker)?;
    let _runtime_guard = tokio.enter();
    let mut runtime = create_worker_runtime(&entrypoint, loader, filesystem)?;
    let isolate = runtime.js_runtime_mut().v8_isolate().thread_safe_handle();
    if control.install_isolate(isolate) {
        let _ = started.send(Err(PluginInvocationError::cancelled()));
        return Ok(());
    }
    let plugin = invocation.plugin.clone();
    let console = invocation
        .arguments
        .get("console")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let retained = match tokio.block_on(crate::deno::execute_retained_emulator(
        runtime.js_runtime_mut(),
        invocation,
        explicit_default,
    )) {
        Ok(retained) if retained.controller_changes => retained,
        Ok(retained) => {
            crate::deno::discard_configuration(runtime.js_runtime_mut(), retained.configuration_id);
            let _ = started.send(Err(PluginInvocationError::worker(
                "onControllerChanged disappeared after capability discovery",
            )));
            return Ok(());
        }
        Err(error) => {
            let _ = started.send(Err(PluginInvocationError::worker(error)));
            return Ok(());
        }
    };
    crate::controller::configure(runtime.js_runtime_mut(), connected);
    let configuration_id = retained.configuration_id;
    let mut launch = retained.launch;
    launch.gamepad_mapping = Some(gamepad_mapping);
    if started.send(Ok(launch)).is_err() {
        crate::deno::discard_configuration(runtime.js_runtime_mut(), configuration_id);
        return Ok(());
    }

    while let Ok(command) = commands.recv() {
        match command {
            crate::controller::ControllerSessionCommand::Attach(sink, response) => {
                let result = crate::controller::attach(runtime.js_runtime_mut(), sink);
                if let Err(error) = &result {
                    errors.lock().push(error.clone());
                }
                let _ = response.send(result);
            }
            crate::controller::ControllerSessionCommand::Changed(topology) => {
                let result = (|| {
                    let assignments = crate::controller::port_assignments(&topology)
                        .map_err(PluginInvocationError::worker)?;
                    let configuration = plugin
                        .materialize(&console, assignments)
                        .map_err(PluginInvocationError::worker)?;
                    crate::deno::rematerialize_configuration(
                        runtime.js_runtime_mut(),
                        configuration_id,
                        configuration,
                    )
                    .map_err(PluginInvocationError::worker)?;
                    let mappings = topology
                        .get("controllerMappings")
                        .cloned()
                        .unwrap_or(topology);
                    let states = crate::controller::states(runtime.js_runtime_mut());
                    tokio
                        .block_on(crate::deno::execute_controller_change(
                            runtime.js_runtime_mut(),
                            &mappings,
                            &states,
                        ))
                        .map_err(PluginInvocationError::worker)?;
                    crate::controller::flush(runtime.js_runtime_mut())
                        .map_err(PluginInvocationError::worker)
                })();
                if let Err(error) = result {
                    crate::controller::discard(runtime.js_runtime_mut());
                    tracing::error!("snowglass: onControllerChanged failed: {error}");
                    errors.lock().push(error.to_string());
                }
            }
            crate::controller::ControllerSessionCommand::Stop => break,
        }
    }
    crate::deno::discard_configuration(runtime.js_runtime_mut(), configuration_id);
    Ok(())
}

fn filesystem_options(
    plugin: &dyn EmulatorPluginInstance,
    storage: StorageFactories,
) -> Result<crate::filesystem::FilesystemOptions, crate::PackageError> {
    let mut options =
        crate::filesystem::FilesystemOptions::new(plugin.module_loader().root().to_owned());
    options.storage = storage;
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

async fn has_callable_controller_export(
    runtime: &mut deno_core::JsRuntime,
    entrypoint: &str,
) -> Result<bool, PluginInvocationError> {
    let entrypoint = serde_json::to_string(entrypoint).map_err(PluginInvocationError::worker)?;
    let script = format!(
        r#"(async () => {{
          const module = await import({entrypoint});
          return typeof module.onControllerChanged === "function"
            || typeof module.default?.onControllerChanged === "function";
        }})()"#
    );
    let promise = runtime
        .execute_script("snowglass:pluginhost/controller-capability.js", script)
        .map_err(PluginInvocationError::worker)?;
    #[allow(deprecated)]
    let resolved = runtime
        .resolve_value(promise)
        .await
        .map_err(PluginInvocationError::worker)?;
    deno_core::scope!(scope, runtime);
    Ok(v8::Local::new(scope, resolved).boolean_value(scope))
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

                [input]
                driver = "sdl3"
                device = "SDL-{index}"

                [input.mapping]
                [input.layouts]
                [platforms.TEST.ports]
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
                    const firstRender = args.configuration.render(args.gamepadBindings);
                    const secondRender = args.configuration.render(args.gamepadBindings);
                    if (firstRender === secondRender || firstRender["settings.ini"] === secondRender["settings.ini"]) {
                      throw new Error("configuration.render() did not return fresh values");
                    }
                    args.filesystem.mountTree("/Emulator", args.resources.runtime, { mode: "rx" });
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
                    hooks: ["vulkan"],
                    graphTracking: true,
                    surfaces: [{ index: 0 }],
                  };
                }
                export function onControllerChanged(context) {
                  context.configuration.options().general.enabled = false;
                  for (const [name, bytes] of Object.entries(context.configuration.render([]))) {
                    context.filesystem.mountFile(`/Emulator/Config/${name}`, bytes, { mode: "r" });
                  }
                }
                export default {
                  fallback(value) { return `fallback:${value}`; },
                  nonFunction: 23,
                  setupLaunch,
                  onControllerChanged,
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
        realized.instantiate("TEST", BTreeMap::new()).unwrap()
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
                    "virtualize": true,
                    "gamepadBindings": []
                }),
                configuration: configuration(),
                storage: BTreeMap::new(),
            })
            .await
            .unwrap();
        let filesystem = result
            .filesystem
            .as_ref()
            .expect("plugin populated a VFS plan");
        assert_eq!(filesystem.filesystem().compiled().bindings().len(), 4);
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
                    "gamepadBindings": [],
                })],
            )
            .await
            .unwrap();
        assert_eq!(result["command"], "emulator.exe");
        assert_eq!(result["graphTracking"], true);

        let explicit_default = runtime
            .handle()
            .invoke(
                "worker-test",
                &["default".to_owned(), "setupLaunch".to_owned()],
                vec![serde_json::json!({
                    "launchPath": "game",
                    "console": "TEST",
                    "portAssignments": {},
                    "gamepadBindings": [],
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
            .invoke_with_storage_owned(
                "worker-test",
                &["setupLaunch".to_owned()],
                vec![serde_json::json!({
                    "launchPath": root.path().join("game.iso"),
                    "console": "TEST",
                    "portAssignments": {},
                    "gamepadBindings": [],
                    "virtualize": true,
                })],
                BTreeMap::new(),
            )
            .await
            .unwrap();
        assert!(result.value["preparedFilesystem"].is_null());
        let filesystem = result.prepared_filesystem.unwrap();
        assert_eq!(filesystem.filesystem().compiled().bindings().len(), 4);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn controller_callback_regenerates_a_live_mounted_configuration() {
        let (root, registry) = package();
        let runtime = PluginRuntime::new(registry, PluginRuntimeOptions::default());
        let result = runtime
            .handle()
            .invoke_with_storage_owned(
                "worker-test",
                &["setupLaunch".to_owned()],
                vec![serde_json::json!({
                    "launchPath": root.path().join("game.iso"),
                    "console": "TEST",
                    "portAssignments": {},
                    "gamepadBindings": [],
                    "virtualize": true,
                })],
                BTreeMap::new(),
            )
            .await
            .unwrap();
        let filesystem = result.prepared_filesystem.unwrap();
        let session = result.controller_session.unwrap();
        filesystem.filesystem().materialize().unwrap();
        session.attach(|_| Ok(())).unwrap();
        session
            .changed(serde_json::json!({ "controllerMappings": [] }))
            .unwrap();

        let path = "/Emulator/Config/settings.ini".parse().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let rendered = loop {
            let file = filesystem
                .filesystem()
                .open(
                    &path,
                    snowglass_vfs::OpenOptions {
                        read: true,
                        write: false,
                        truncate: false,
                    },
                )
                .unwrap();
            let mut bytes = [0; 128];
            let length = file.read(0, &mut bytes).unwrap();
            let rendered = String::from_utf8_lossy(&bytes[..length]).into_owned();
            if rendered.contains("Enabled=false") || std::time::Instant::now() >= deadline {
                break rendered;
            }
            std::thread::yield_now();
        };
        assert!(rendered.contains("Enabled=false"), "{rendered}");
        assert!(session.take_errors().is_empty());
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
}
