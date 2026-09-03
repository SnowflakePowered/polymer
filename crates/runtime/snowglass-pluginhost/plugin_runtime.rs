use crate::filesystem::StorageFactories;
use crate::{
    EmulatorLaunchArguments, EmulatorLaunchInvocation, EmulatorPluginInstance, PluginRegistry,
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
use tokio::sync::{Notify, OnceCell, Semaphore};

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

struct IsolateDeadline {
    completed: std::sync::mpsc::SyncSender<()>,
    expired: Arc<AtomicBool>,
    watchdog: thread::JoinHandle<()>,
}

impl IsolateDeadline {
    fn start(
        isolate: v8::IsolateHandle,
        timeout: std::time::Duration,
    ) -> Result<Self, PluginInvocationError> {
        let (completed, receiver) = std::sync::mpsc::sync_channel(1);
        let expired = Arc::new(AtomicBool::new(false));
        let watchdog_expired = expired.clone();
        let watchdog = thread::Builder::new()
            .name("snowglass-plugin-deadline".to_owned())
            .spawn(move || {
                if receiver.recv_timeout(timeout).is_err() {
                    watchdog_expired.store(true, Ordering::Release);
                    isolate.terminate_execution();
                }
            })
            .map_err(PluginInvocationError::worker)?;
        Ok(Self {
            completed,
            expired,
            watchdog,
        })
    }

    fn finish(self, runtime: &mut deno_core::JsRuntime) -> bool {
        let _ = self.completed.send(());
        let _ = self.watchdog.join();
        let expired = self.expired.load(Ordering::Acquire);
        if expired {
            runtime.v8_isolate().cancel_terminate_execution();
        }
        expired
    }
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
    plugins: Mutex<HashMap<Box<str>, Arc<PluginLifecycle>>>,
    shutdown: AtomicBool,
}

struct PluginLifecycle {
    storage: Arc<crate::deno::plugin_storage::PluginStorage>,
    loaded: OnceCell<()>,
}

impl PluginLifecycle {
    fn new() -> Self {
        Self {
            storage: Arc::new(crate::deno::plugin_storage::PluginStorage::default()),
            loaded: OnceCell::new(),
        }
    }

    async fn load(
        &self,
        runtime: &mut snowglass_deno::Runtime,
        entrypoint: &str,
    ) -> Result<(), PluginInvocationError> {
        self.loaded
            .get_or_try_init(|| async {
                let result = invoke_optional_export(
                    runtime.js_runtime_mut(),
                    entrypoint,
                    "onPluginLoad",
                    Vec::new(),
                )
                .await;
                if result.is_err() {
                    self.storage.clear();
                }
                result.map(|_| ())
            })
            .await
            .map(|_| ())
    }
}

enum ExportInvocation {
    EmulatorLaunch { explicit_default: bool },
    Generic,
}

impl ExportInvocation {
    fn parse(path: &[String]) -> Result<Self, PluginInvocationError> {
        if path.is_empty() || path.iter().any(String::is_empty) {
            return Err(PluginInvocationError::new(
                "TypeError",
                "INVALID_EXPORT_PATH",
                "plugin export path must contain non-empty property names",
            ));
        }
        Ok(match path {
            [name] if name == "onLaunch" => Self::EmulatorLaunch {
                explicit_default: false,
            },
            [root, name] if root == "default" && name == "onLaunch" => Self::EmulatorLaunch {
                explicit_default: true,
            },
            _ => Self::Generic,
        })
    }
}

struct PreparedEmulatorLaunch {
    invocation: EmulatorLaunchInvocation,
    topology: snowglass_controllers::ControllerTopology,
}

impl PreparedEmulatorLaunch {
    fn new(
        plugin: Arc<dyn EmulatorPluginInstance>,
        arguments: Vec<Value>,
        storage: StorageFactories,
        quick_resume: Option<crate::QuickResumeData>,
    ) -> Result<Self, PluginInvocationError> {
        let [mut arguments]: [Value; 1] = arguments.try_into().map_err(|_| {
            PluginInvocationError::new(
                "TypeError",
                "INVALID_ARGUMENTS",
                "onLaunch expects exactly one argument object",
            )
        })?;
        let object = arguments.as_object_mut().ok_or_else(|| {
            PluginInvocationError::new(
                "TypeError",
                "INVALID_ARGUMENTS",
                "onLaunch expects exactly one argument object",
            )
        })?;
        let console = object
            .get("console")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                PluginInvocationError::new(
                    "TypeError",
                    "INVALID_ARGUMENTS",
                    "onLaunch arguments require a string console",
                )
            })?
            .to_owned();
        let topology = serde_json::from_value::<snowglass_controllers::ControllerTopology>(
            object.get("controllerTopology").cloned().ok_or_else(|| {
                PluginInvocationError::new(
                    "TypeError",
                    "INVALID_ARGUMENTS",
                    "onLaunch arguments require controllerTopology",
                )
            })?,
        )
        .map_err(|error| {
            PluginInvocationError::new(
                "TypeError",
                "INVALID_ARGUMENTS",
                format!("invalid onLaunch controllerTopology: {error}"),
            )
        })?;
        let assignments = topology
            .port_assignments()
            .map_err(PluginInvocationError::worker)?;
        let configuration = plugin
            .materialize(&console, assignments)
            .map_err(PluginInvocationError::worker)?;
        object.remove("storage");
        Ok(Self {
            invocation: EmulatorLaunchInvocation {
                plugin,
                arguments,
                configuration,
                storage,
                quick_resume,
            },
            topology,
        })
    }
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
                    plugins: Mutex::new(HashMap::new()),
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

    fn lifecycle(&self, plugin_id: &str) -> Arc<PluginLifecycle> {
        self.state
            .plugins
            .lock()
            .entry(plugin_id.into())
            .or_insert_with(|| Arc::new(PluginLifecycle::new()))
            .clone()
    }

    pub async fn configure(
        &self,
        plugin_id: &str,
        game: Value,
        user_configuration: Option<Value>,
    ) -> Result<Option<serde_json::Value>, PluginInvocationError> {
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
        let mut arguments = vec![game];
        if let Some(configuration) = user_configuration {
            arguments.push(configuration);
        }
        let cancellation = self.cancellation_token();
        let entrypoint = plugin.entrypoint().to_string();
        let invocation_entrypoint = entrypoint.clone();
        let loader = plugin.module_loader().clone();
        let filesystem = filesystem_options(plugin.as_ref(), BTreeMap::new(), None)
            .map_err(PluginInvocationError::worker)?;
        let lifecycle = self.lifecycle(plugin_id);
        let result = self
            .run_with_control(cancellation.control, move |control| {
                run_worker(
                    entrypoint,
                    loader,
                    filesystem,
                    lifecycle,
                    control,
                    move |runtime| async move {
                        let mut runtime = runtime;
                        invoke_optional_export(
                            runtime.js_runtime_mut(),
                            &invocation_entrypoint,
                            "onConfigure",
                            arguments,
                        )
                        .await
                    },
                )
            })
            .await?;
        let Some(value) = result else {
            return Ok(None);
        };
        plugin
            .configuration_definition()
            .expand_layout(value)
            .map(Some)
            .map_err(PluginInvocationError::worker)
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
            .invoke_inner(
                plugin_id,
                export_path,
                arguments,
                storage,
                None,
                cancellation,
            )
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
        self.invoke_inner(
            plugin_id,
            export_path,
            arguments,
            storage,
            None,
            cancellation,
        )
        .await
    }

    pub async fn invoke_emulator_with_storage_owned(
        &self,
        plugin_id: &str,
        export_path: &[String],
        arguments: Vec<Value>,
        storage: StorageFactories,
        quick_resume: Option<crate::QuickResumeData>,
    ) -> Result<PluginInvocationResult, PluginInvocationError> {
        let cancellation = self.cancellation_token();
        self.invoke_inner(
            plugin_id,
            export_path,
            arguments,
            storage,
            quick_resume,
            cancellation,
        )
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
                None,
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
        quick_resume: Option<crate::QuickResumeData>,
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
        match ExportInvocation::parse(export_path)? {
            ExportInvocation::EmulatorLaunch { explicit_default } => {
                let setup = PreparedEmulatorLaunch::new(plugin, arguments, storage, quick_resume)?;
                return self
                    .invoke_emulator_launch(setup, cancellation, explicit_default)
                    .await;
            }
            ExportInvocation::Generic => {}
        }
        self.invoke_generic(plugin, export_path, arguments, cancellation)
            .await
    }

    async fn invoke_emulator_launch(
        &self,
        setup: PreparedEmulatorLaunch,
        cancellation: PluginCancellationToken,
        explicit_default: bool,
    ) -> Result<PluginInvocationResult, PluginInvocationError> {
        let (mut result, session) = self
            .emulator_launch_with_cancellation(
                setup.invocation,
                cancellation,
                explicit_default,
                setup.topology.clone(),
            )
            .await?;
        let filesystem = result.filesystem.take();
        let retained_session = result.session.take().or(session);
        if result.controller_topology.is_none() {
            result.controller_topology = Some(setup.topology);
        }
        result.prepared_filesystem = None;
        let value = serde_json::to_value(result).map_err(PluginInvocationError::worker)?;
        Ok(PluginInvocationResult {
            value,
            prepared_filesystem: filesystem,
            controller_session: retained_session,
        })
    }

    async fn invoke_generic(
        &self,
        plugin: Arc<dyn EmulatorPluginInstance>,
        export_path: &[String],
        arguments: Vec<Value>,
        cancellation: PluginCancellationToken,
    ) -> Result<PluginInvocationResult, PluginInvocationError> {
        let entrypoint = plugin.entrypoint().to_string();
        let invocation_entrypoint = entrypoint.clone();
        let loader = plugin.module_loader().clone();
        let filesystem = filesystem_options(plugin.as_ref(), BTreeMap::new(), None)
            .map_err(PluginInvocationError::worker)?;
        let lifecycle = self.lifecycle(plugin.id());
        let path = export_path.to_vec();
        let value = self
            .run_with_control(cancellation.control, move |control| {
                run_worker(
                    entrypoint,
                    loader,
                    filesystem,
                    lifecycle,
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

    async fn emulator_launch_with_cancellation(
        &self,
        invocation: EmulatorLaunchInvocation,
        cancellation: PluginCancellationToken,
        explicit_default: bool,
        topology: snowglass_controllers::ControllerTopology,
    ) -> Result<(EmulatorLaunchArguments, Option<crate::ControllerSession>), PluginInvocationError>
    {
        let filesystem = filesystem_options(
            invocation.plugin.as_ref(),
            invocation.storage.clone(),
            invocation.quick_resume.clone(),
        )
        .map_err(PluginInvocationError::worker)?;
        let lifecycle = self.lifecycle(invocation.plugin.id());
        self.run_with_control(cancellation.control, move |control| {
            start_controller_session_worker(
                invocation,
                explicit_default,
                filesystem,
                lifecycle,
                control,
                topology,
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

fn create_worker_runtime(
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

fn start_controller_session_worker(
    invocation: EmulatorLaunchInvocation,
    explicit_default: bool,
    filesystem: crate::filesystem::FilesystemOptions,
    lifecycle: Arc<PluginLifecycle>,
    control: Arc<InvocationControl>,
    topology: snowglass_controllers::ControllerTopology,
) -> Result<(EmulatorLaunchArguments, Option<crate::ControllerSession>), PluginInvocationError> {
    let entrypoint = invocation.plugin.entrypoint().to_string();
    let loader = invocation.plugin.module_loader().clone();
    let (commands, command_receiver) = std::sync::mpsc::channel();
    let (started, startup) = std::sync::mpsc::sync_channel(1);
    thread::Builder::new()
        .name(format!(
            "snowglass-plugin-session-{}",
            invocation.plugin.id()
        ))
        .spawn(move || {
            let result = controller_session_worker(ControllerSessionWorkerArgs {
                entrypoint,
                loader,
                filesystem,
                lifecycle,
                control,
                invocation,
                explicit_default,
                topology,
                commands: command_receiver,
                started,
            });
            if let Err(error) = result {
                tracing::error!("snowglass: controller plugin session failed: {error}");
            }
        })
        .map_err(PluginInvocationError::worker)?;
    let startup = startup.recv().map_err(|_| {
        PluginInvocationError::worker("controller plugin session failed to start")
    })??;
    let session = startup
        .retained
        .then(|| crate::ControllerSession::new(commands));
    Ok((startup.launch, session))
}

struct ControllerWorkerStartup {
    launch: EmulatorLaunchArguments,
    retained: bool,
}

struct ControllerSessionWorkerArgs {
    entrypoint: String,
    loader: crate::PluginModuleLoader,
    filesystem: crate::filesystem::FilesystemOptions,
    lifecycle: Arc<PluginLifecycle>,
    control: Arc<InvocationControl>,
    invocation: EmulatorLaunchInvocation,
    explicit_default: bool,
    topology: snowglass_controllers::ControllerTopology,
    commands: std::sync::mpsc::Receiver<crate::controller::ControllerSessionCommand>,
    started: std::sync::mpsc::SyncSender<Result<ControllerWorkerStartup, PluginInvocationError>>,
}

struct ControllerSessionWorker {
    tokio: tokio::runtime::Runtime,
    runtime: snowglass_deno::Runtime,
    plugin: Arc<dyn EmulatorPluginInstance>,
    console: String,
    configuration_id: crate::configuration::ConfigurationId,
    commands: std::sync::mpsc::Receiver<crate::controller::ControllerSessionCommand>,
}

fn controller_session_worker(
    args: ControllerSessionWorkerArgs,
) -> Result<(), PluginInvocationError> {
    if let Some(worker) = ControllerSessionWorker::start(args)? {
        worker.run();
    }
    Ok(())
}

impl ControllerSessionWorker {
    fn start(args: ControllerSessionWorkerArgs) -> Result<Option<Self>, PluginInvocationError> {
        let tokio = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(PluginInvocationError::worker)?;
        let mut runtime = {
            let _runtime_context = tokio.enter();
            create_worker_runtime(
                &args.entrypoint,
                args.loader,
                args.filesystem,
                args.lifecycle.storage.clone(),
            )?
        };
        let isolate = runtime.js_runtime_mut().v8_isolate().thread_safe_handle();
        if args.control.install_isolate(isolate) {
            let _ = args.started.send(Err(PluginInvocationError::cancelled()));
            return Ok(None);
        }
        if let Err(error) = tokio.block_on(args.lifecycle.load(&mut runtime, &args.entrypoint)) {
            let _ = args.started.send(Err(error));
            return Ok(None);
        }
        let plugin = args.invocation.plugin.clone();
        let console = args.invocation.arguments["console"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let retained = match tokio.block_on(crate::deno::execute_retained_emulator(
            runtime.js_runtime_mut(),
            args.invocation,
            args.explicit_default,
        )) {
            Ok(retained) => retained,
            Err(error) => {
                let _ = args.started.send(Err(PluginInvocationError::worker(error)));
                return Ok(None);
            }
        };
        let controller_changes = retained.controller_changes;
        let retained_session = controller_changes
            || retained.pause_resume
            || retained.state_save
            || retained.state_resume
            || retained.quit;
        let configuration_id = retained.configuration_id;
        let mut launch = retained.launch;
        let applied_topology = launch
            .controller_topology
            .clone()
            .unwrap_or_else(|| args.topology.clone());
        tracing::info!(
            plugin = %plugin.id(),
            thread = std::thread::current().name().unwrap_or("unnamed"),
            applied = %applied_topology.assignment_diagnostics(),
            "snowglass: controller plugin session initialized"
        );
        launch.controller_topology = Some(applied_topology.clone());
        if args
            .started
            .send(Ok(ControllerWorkerStartup {
                launch,
                retained: retained_session,
            }))
            .is_err()
            || !retained_session
        {
            crate::deno::discard_configuration(runtime.js_runtime_mut(), configuration_id);
            return Ok(None);
        }
        Ok(Some(Self {
            tokio,
            runtime,
            plugin,
            console,
            configuration_id,
            commands: args.commands,
        }))
    }

    fn run(mut self) {
        while let Ok(command) = self.commands.recv() {
            match command {
                crate::controller::ControllerSessionCommand::AttachAutomation(
                    session,
                    response,
                ) => {
                    crate::automation::attach(self.runtime.js_runtime_mut(), session);
                    let _ = response.send(Ok(()));
                }
                crate::controller::ControllerSessionCommand::AttachActions(sink, response) => {
                    crate::actions::attach(self.runtime.js_runtime_mut(), sink);
                    let _ = response.send(Ok(()));
                }
                crate::controller::ControllerSessionCommand::Changed(change, response) => {
                    self.change(&change, response);
                }
                crate::controller::ControllerSessionCommand::PauseResume(
                    state,
                    timeout,
                    response,
                ) => {
                    let _ = response.send(self.pause_resume(state, timeout));
                }
                crate::controller::ControllerSessionCommand::StateAction(
                    slot,
                    save,
                    timeout,
                    response,
                ) => {
                    let _ = response.send(self.state_action(slot, save, timeout));
                }
                crate::controller::ControllerSessionCommand::Quit(timeout, response) => {
                    let _ = response.send(self.quit(timeout));
                }
                crate::controller::ControllerSessionCommand::Stop => break,
            }
        }
        crate::automation::close(self.runtime.js_runtime_mut());
        crate::actions::close(self.runtime.js_runtime_mut());
        crate::deno::discard_configuration(self.runtime.js_runtime_mut(), self.configuration_id);
    }

    fn pause_resume(
        &mut self,
        state: crate::PauseState,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        let started = std::time::Instant::now();
        let thread = std::thread::current();
        tracing::info!(
            plugin = %self.plugin.id(),
            ?state,
            thread = thread.name().unwrap_or("unnamed"),
            "snowglass: pause/resume plugin callback started"
        );
        let result = self.tokio.block_on(async {
            tokio::time::timeout(
                timeout,
                crate::deno::execute_pause_resume(self.runtime.js_runtime_mut(), state),
            )
            .await
            .map_err(|_| {
                format!(
                    "onPauseResume exceeded its {}ms deadline",
                    timeout.as_millis()
                )
            })?
            .map_err(|error| error.to_string())
        });
        if let Err(error) = &result {
            tracing::error!("snowglass: onPauseResume failed: {error}");
        } else {
            tracing::info!(
                plugin = %self.plugin.id(),
                ?state,
                thread = thread.name().unwrap_or("unnamed"),
                elapsed_ms = started.elapsed().as_millis(),
                "snowglass: pause/resume plugin callback completed"
            );
        }
        result
    }

    fn state_action(
        &mut self,
        slot: u32,
        save: bool,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        let callback = if save { "onStateSave" } else { "onStateResume" };
        let timeout_error = format!("{callback} exceeded its {}ms deadline", timeout.as_millis());
        let deadline = IsolateDeadline::start(
            self.runtime
                .js_runtime_mut()
                .v8_isolate()
                .thread_safe_handle(),
            timeout,
        )
        .map_err(|error| error.to_string())?;
        let result = self.tokio.block_on(async {
            tokio::time::timeout(
                timeout,
                crate::deno::execute_state_action(self.runtime.js_runtime_mut(), slot, save),
            )
            .await
            .map_err(|_| timeout_error.clone())?
            .map_err(|error| error.to_string())
        });
        let result = if deadline.finish(self.runtime.js_runtime_mut()) {
            Err(timeout_error)
        } else {
            result
        };
        if let Err(error) = &result {
            tracing::error!(plugin = %self.plugin.id(), slot, "snowglass: {callback} failed: {error}");
        }
        result
    }

    fn quit(&mut self, timeout: std::time::Duration) -> Result<Option<Vec<u8>>, String> {
        let timeout_error = format!("onQuit exceeded its {}ms deadline", timeout.as_millis());
        let deadline = IsolateDeadline::start(
            self.runtime
                .js_runtime_mut()
                .v8_isolate()
                .thread_safe_handle(),
            timeout,
        )
        .map_err(|error| error.to_string())?;
        let result = self.tokio.block_on(async {
            tokio::time::timeout(
                timeout,
                crate::deno::execute_quit(self.runtime.js_runtime_mut()),
            )
            .await
            .map_err(|_| timeout_error.clone())?
            .map_err(|error| error.to_string())
        });
        let result = if deadline.finish(self.runtime.js_runtime_mut()) {
            Err(timeout_error)
        } else {
            result
        };
        if let Err(error) = &result {
            tracing::error!(plugin = %self.plugin.id(), "snowglass: onQuit failed: {error}");
        }
        result
    }

    fn change(
        &mut self,
        change: &snowglass_controllers::ControllerTopologyChange,
        response: tokio::sync::oneshot::Sender<
            Result<snowglass_controllers::ControllerTopology, String>,
        >,
    ) {
        let started = std::time::Instant::now();
        let thread = std::thread::current();
        tracing::info!(
            plugin = %self.plugin.id(),
            thread = thread.name().unwrap_or("unnamed"),
            previous = %change.previous.assignment_diagnostics(),
            requested = %change.requested.assignment_diagnostics(),
            "snowglass: controller plugin callback started"
        );
        let result = self.apply_change(change);
        match result {
            Ok(applied) => {
                tracing::info!(
                    plugin = %self.plugin.id(),
                    thread = thread.name().unwrap_or("unnamed"),
                    elapsed_ms = started.elapsed().as_millis(),
                    applied = %applied.assignment_diagnostics(),
                    "snowglass: controller plugin callback completed"
                );
                let _ = response.send(Ok(applied));
            }
            Err(error) => {
                crate::filesystem::discard_staged(self.runtime.js_runtime_mut());
                tracing::error!(
                    plugin = %self.plugin.id(),
                    thread = thread.name().unwrap_or("unnamed"),
                    elapsed_ms = started.elapsed().as_millis(),
                    requested = %change.requested.assignment_diagnostics(),
                    "snowglass: onControllerChanged failed: {error}"
                );
                let _ = response.send(Err(error.to_string()));
            }
        }
    }

    fn apply_change(
        &mut self,
        change: &snowglass_controllers::ControllerTopologyChange,
    ) -> Result<snowglass_controllers::ControllerTopology, PluginInvocationError> {
        let checkpoint = crate::deno::checkpoint_configuration(
            self.runtime.js_runtime_mut(),
            self.configuration_id,
        )
        .map_err(PluginInvocationError::worker)?;
        let result = self.apply_change_inner(change);
        if result.is_err() {
            crate::deno::restore_configuration(
                self.runtime.js_runtime_mut(),
                self.configuration_id,
                checkpoint,
            );
            if let Err(error) =
                crate::deno::refresh_configuration_mounts(self.runtime.js_runtime_mut())
                    .map_err(PluginInvocationError::worker)
                    .and_then(|()| {
                        crate::filesystem::flush_staged(self.runtime.js_runtime_mut())
                            .map_err(PluginInvocationError::worker)
                    })
            {
                tracing::error!(
                    plugin = %self.plugin.id(),
                    "snowglass: restoring controller configuration files failed: {error}"
                );
            }
        }
        result
    }

    fn apply_change_inner(
        &mut self,
        change: &snowglass_controllers::ControllerTopologyChange,
    ) -> Result<snowglass_controllers::ControllerTopology, PluginInvocationError> {
        let assignments = change
            .requested
            .port_assignments()
            .map_err(PluginInvocationError::worker)?;
        let configuration = self
            .plugin
            .materialize(&self.console, assignments)
            .map_err(PluginInvocationError::worker)?;
        crate::deno::rematerialize_configuration(
            self.runtime.js_runtime_mut(),
            self.configuration_id,
            configuration,
            &change.requested,
        )
        .map_err(PluginInvocationError::worker)?;
        let output = self.execute_callback(change)?;
        crate::filesystem::flush_staged(self.runtime.js_runtime_mut())
            .map_err(PluginInvocationError::worker)?;
        Ok(output.controller_topology)
    }

    fn execute_callback(
        &mut self,
        change: &snowglass_controllers::ControllerTopologyChange,
    ) -> Result<crate::deno::ControllerChangeOutput, PluginInvocationError> {
        const TIMEOUT: std::time::Duration =
            snowglass_controllers::DEFAULT_CONTROLLER_TOPOLOGY_TRANSACTION_TIMEOUT;
        const TIMEOUT_ERROR: &str = "onControllerChanged exceeded its 60 second deadline";
        let deadline = IsolateDeadline::start(
            self.runtime
                .js_runtime_mut()
                .v8_isolate()
                .thread_safe_handle(),
            TIMEOUT,
        )?;
        let callback = self.tokio.block_on(async {
            tokio::time::timeout(
                TIMEOUT,
                crate::deno::execute_controller_change(
                    self.runtime.js_runtime_mut(),
                    &change.requested,
                    &change.previous,
                ),
            )
            .await
            .map_err(|_| PluginInvocationError::worker(TIMEOUT_ERROR))?
            .map_err(PluginInvocationError::worker)
        });
        if deadline.finish(self.runtime.js_runtime_mut()) {
            return Err(PluginInvocationError::worker(TIMEOUT_ERROR));
        }
        callback
    }
}

fn filesystem_options(
    plugin: &dyn EmulatorPluginInstance,
    storage: StorageFactories,
    quick_resume: Option<crate::QuickResumeData>,
) -> Result<crate::filesystem::FilesystemOptions, crate::PackageError> {
    let mut options =
        crate::filesystem::FilesystemOptions::new(plugin.module_loader().root().to_owned());
    options.storage = storage;
    options.quick_resume = quick_resume;
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

async fn invoke_optional_export(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex as StdMutex},
        time::Duration,
    };
    use tempfile::TempDir;

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

                [actions]
                target = "settings"

                [actions.map]
                pause = { name = "TogglePause", expression = "F10" }

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
                import runtime from "resources:runtime";
                import { pluginStorage } from "snowglass:pluginStorage";
                let isolateCounter = 0;
                export function onPluginLoad() {
                  const loads = pluginStorage.getItem("loads") ?? 0;
                  pluginStorage.setItem("loads", loads + 1);
                  pluginStorage.setItem("cached", { names: ["graphics-pack"] });
                }
                export async function onConfigure(game, userConfiguration) {
                  const entries = [];
                  for await (const entry of runtime) {
                    entries.push(entry.name);
                    if (entry.kind === "file") await entry.bytes();
                  }
                  return {
                    schema: "worker/v1",
                    key: game.launchPath,
                    groups: [{
                      key: "general",
                      title: entries.join(","),
                      options: [{ key: "enabled", source: "general.enabled" }],
                    }],
                    values: {
                      general: { enabled: userConfiguration?.general?.enabled === true },
                    },
                  };
                }
                export function named(value) {
                  isolateCounter++;
                  return { value, isolateCounter };
                }
                export function cached() {
                  const value = pluginStorage.getItem("cached");
                  value.names.push("isolate");
                  return { loads: pluginStorage.getItem("loads"), value };
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
                export function onLaunch(args) {
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
                    hooks: { graphics: ["vulkan"], input: ["sdl3"] },
                    graphTracking: true,
                    surfaces: [{ index: 0 }],
                  };
                }
                export async function onControllerChanged(context) {
                  if (typeof context.actions.pause?.activate !== "function") {
                    throw new Error("onControllerChanged did not receive configured actions");
                  }
                  const window = await context.ui.waitForWindow({ pattern: "Emulator" });
                  await window.locator('button[name="Controllers"]').press();
                  context.configuration.options().general.enabled = false;
                  for (const [name, bytes] of Object.entries(context.configuration.render())) {
                    context.filesystem.mountFile(`/Emulator/Config/${name}`, bytes, { mode: "r" });
                  }
                }
                export function onParseHotkey(expression) {
                  return { kind: "keyboard", code: expression, modifiers: ["Control"] };
                }
                export function onPauseResume(context) {
                  if (context.pauseState !== "pause" && context.pauseState !== "resume") {
                    throw new Error(`invalid pause state: ${context.pauseState}`);
                  }
                  context.actions.pause.activate();
                }
                export default {
                  fallback(value) { return `fallback:${value}`; },
                  nonFunction: 23,
                  onLaunch,
                  onControllerChanged,
                  onParseHotkey,
                  onPauseResume,
                };
            "#,
        )
        .unwrap();
        let registry = PluginRegistry::new();
        registry.load(root.path()).unwrap();
        (root, registry)
    }

    fn configuration_mount_package() -> (TempDir, PluginRegistry) {
        let (root, _) = package();
        std::fs::write(
            root.path().join("plugin.js"),
            r#"
                export function onParseHotkey(expression) {
                  return { kind: "keyboard", code: expression };
                }
                export function onLaunch(args) {
                  args.filesystem.mountTree("/Emulator", args.resources.runtime, { mode: "rx" });
                  args.filesystem.mountConfiguration("/Emulator/Config", args.configuration, { mode: "r" });
                  return {
                    command: args.filesystem.path("/Emulator/emulator.exe"),
                    argv: [],
                    hooks: { graphics: [], input: ["sdl3"] },
                    graphTracking: false,
                    surfaces: [],
                  };
                }
            "#,
        )
        .unwrap();
        let registry = PluginRegistry::new();
        registry.load(root.path()).unwrap();
        (root, registry)
    }

    struct RecordingAutomation(Arc<StdMutex<Vec<&'static str>>>);

    impl snowglass_automation::AutomationDriver for RecordingAutomation {
        fn execute(
            &mut self,
            request: snowglass_automation::AutomationRequest,
        ) -> Result<snowglass_automation::AutomationResponse, snowglass_automation::AutomationError>
        {
            use snowglass_automation::{AutomationRequest, AutomationResponse};
            match request {
                AutomationRequest::WaitForWindow { .. } => {
                    self.0.lock().unwrap().push("window");
                    Ok(AutomationResponse::Window(
                        snowglass_automation::WindowSnapshot {
                            id: snowglass_automation::WindowId("test-window".to_owned()),
                            process_id: 7,
                            title: "Emulator".to_owned(),
                            class_name: "TestWindow".to_owned(),
                            visible: true,
                        },
                    ))
                }
                AutomationRequest::LocatorAction { .. } => {
                    self.0.lock().unwrap().push("action");
                    Ok(AutomationResponse::Unit)
                }
                _ => Err(snowglass_automation::AutomationError::new(
                    "unexpectedRequest",
                    "test received an unexpected automation request",
                )),
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn facade_on_emulator_launch_materializes_its_invocation_configuration() {
        let (_root, registry) = package();
        let runtime = PluginRuntime::new(registry, PluginRuntimeOptions::default());
        let result = runtime
            .handle()
            .invoke(
                "worker-test",
                &["onLaunch".to_owned()],
                vec![serde_json::json!({
                    "launchPath": "game",
                    "console": "TEST",
                    "controllerTopology": { "mappings": [] },
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
                &["default".to_owned(), "onLaunch".to_owned()],
                vec![serde_json::json!({
                    "launchPath": "game",
                    "console": "TEST",
                    "controllerTopology": { "mappings": [] },
                })],
            )
            .await
            .unwrap();
        assert_eq!(explicit_default["command"], "emulator.exe");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn plugin_storage_is_shared_between_fresh_isolates() {
        let (_root, registry) = package();
        let runtime = PluginRuntime::new(registry, PluginRuntimeOptions::default());
        let handle = runtime.handle();
        let path = ["cached".to_owned()];

        for _ in 0..2 {
            assert_eq!(
                handle
                    .invoke("worker-test", &path, Vec::new())
                    .await
                    .unwrap(),
                serde_json::json!({
                    "loads": 1,
                    "value": { "names": ["graphics-pack", "isolate"] },
                })
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prepared_filesystems_are_returned_to_the_application_owner() {
        let (root, registry) = package();
        let runtime = PluginRuntime::new(registry, PluginRuntimeOptions::default());
        let handle = runtime.handle();
        let result = handle
            .invoke_with_storage_owned(
                "worker-test",
                &["onLaunch".to_owned()],
                vec![serde_json::json!({
                    "launchPath": root.path().join("game.iso"),
                    "console": "TEST",
                    "controllerTopology": { "mappings": [] },
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
                &["onLaunch".to_owned()],
                vec![serde_json::json!({
                    "launchPath": root.path().join("game.iso"),
                    "console": "TEST",
                    "controllerTopology": { "mappings": [] },
                    "virtualize": true,
                })],
                BTreeMap::new(),
            )
            .await
            .unwrap();
        let filesystem = result.prepared_filesystem.unwrap();
        let session = result.controller_session.unwrap();
        filesystem.filesystem().materialize().unwrap();
        let automation_calls = Arc::new(StdMutex::new(Vec::new()));
        let automation = snowglass_automation::AutomationSession::spawn(RecordingAutomation(
            automation_calls.clone(),
        ))
        .unwrap();
        session.attach_automation(automation).unwrap();
        session
            .change(snowglass_controllers::ControllerTopologyChange {
                previous: snowglass_controllers::ControllerTopology::default(),
                requested: snowglass_controllers::ControllerTopology::default(),
            })
            .unwrap()
            .await
            .unwrap()
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
        assert_eq!(*automation_calls.lock().unwrap(), ["window", "action"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pause_callback_activates_its_parsed_hotkey_action() {
        let (root, registry) = package();
        let runtime = PluginRuntime::new(registry, PluginRuntimeOptions::default());
        let result = runtime
            .handle()
            .invoke_with_storage_owned(
                "worker-test",
                &["onLaunch".to_owned()],
                vec![serde_json::json!({
                    "launchPath": root.path().join("game.iso"),
                    "console": "TEST",
                    "controllerTopology": { "mappings": [] },
                })],
                BTreeMap::new(),
            )
            .await
            .unwrap();
        let session = result.controller_session.unwrap();
        let activated = Arc::new(StdMutex::new(Vec::new()));
        let recorded = activated.clone();
        session
            .attach_actions(move |hotkey| {
                recorded.lock().unwrap().push(hotkey.clone());
                Ok(())
            })
            .unwrap();
        session
            .pause_resume(crate::PauseState::Pause, None)
            .unwrap()
            .await
            .unwrap()
            .unwrap();
        session
            .pause_resume(crate::PauseState::Resume, None)
            .unwrap()
            .await
            .unwrap()
            .unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while activated.lock().unwrap().len() != 2 && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(
            *activated.lock().unwrap(),
            [
                crate::ParsedHotkey::Keyboard {
                    code: "F10".to_owned(),
                    modifiers: vec!["Control".to_owned()],
                },
                crate::ParsedHotkey::Keyboard {
                    code: "F10".to_owned(),
                    modifiers: vec!["Control".to_owned()],
                },
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn configuration_mount_retains_a_controller_session_without_a_callback() {
        let (root, registry) = configuration_mount_package();
        let runtime = PluginRuntime::new(registry, PluginRuntimeOptions::default());
        let result = runtime
            .handle()
            .invoke_with_storage_owned(
                "worker-test",
                &["onLaunch".to_owned()],
                vec![serde_json::json!({
                    "launchPath": root.path().join("game.iso"),
                    "console": "TEST",
                    "controllerTopology": { "mappings": [] },
                })],
                BTreeMap::new(),
            )
            .await
            .unwrap();
        let filesystem = result.prepared_filesystem.unwrap();
        let session = result.controller_session.unwrap();
        filesystem.filesystem().materialize().unwrap();
        session
            .change(snowglass_controllers::ControllerTopologyChange {
                previous: snowglass_controllers::ControllerTopology::default(),
                requested: snowglass_controllers::ControllerTopology::default(),
            })
            .unwrap()
            .await
            .unwrap()
            .unwrap();

        let path = "/Emulator/Config/settings.ini".parse().unwrap();
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
        assert!(String::from_utf8_lossy(&bytes[..length]).contains("Enabled=false"));
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
