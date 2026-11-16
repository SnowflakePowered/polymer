pub(crate) mod events;
pub(crate) mod jobs;
mod platform;
pub use events::{EventEmitter, PluginEvent};
pub use platform::UpdateTarget;
pub(crate) mod controller_session;
mod invocation;
pub(crate) mod js;
mod worker;

pub(crate) use invocation::InvocationControl;
pub use invocation::{
    PluginCancellationToken, PluginInvocationError, PluginInvocationResult, PluginRuntimeOptions,
};

use crate::{
    controller::ControllerSession,
    emulator::{
        EmulatorLaunchArguments, EmulatorLaunchInvocation, EmulatorPluginInstance, QuickResumeData,
    },
    filesystem::StorageFactories,
    loading::PluginRegistry,
    plugin::PluginInstance,
    scraper::{ScrapeInput, ScrapeResult, ScraperPluginInstance},
};
use controller_session::ControllerActor;
use parking_lot::Mutex;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::{OnceCell, Semaphore};
use worker::{
    WorkerOptions, invoke_export, invoke_optional_export, invoke_scrape_generator, plugin_exports,
    run_worker,
};

struct RuntimeState {
    registry: PluginRegistry,
    directories: polymer_library::Directories,
    permits: Arc<Semaphore>,
    next_call: AtomicU64,
    active: Mutex<HashMap<u64, Arc<InvocationControl>>>,
    plugins: Mutex<HashMap<Box<str>, Arc<PluginLifecycle>>>,
    shutdown: AtomicBool,
    jobs: Option<Arc<polymer_library::jobs::JobStore>>,
}

struct PluginLifecycle {
    events: Arc<events::EventEmitter>,
    jobs: jobs::PluginJobs,
    storage: Arc<crate::plugin_storage::PluginStorage>,
    /// The functions the plugin exports, discovered when it first loads in this process.
    loaded: OnceCell<BTreeSet<String>>,
    /// Whether the plugin's contents are usable: false when `onPluginLoad` returned false,
    /// until `onUpdateContents` succeeds.
    ready: AtomicBool,
    /// Serializes the plugin's bootstraps.
    bootstrap: Arc<tokio::sync::Mutex<()>>,
}

impl PluginLifecycle {
    fn new(plugin: &str, store: Option<Arc<polymer_library::jobs::JobStore>>) -> Self {
        Self {
            events: Arc::new(events::EventEmitter::new(plugin)),
            jobs: jobs::PluginJobs {
                store,
                plugin: plugin.to_owned(),
            },
            storage: Arc::new(crate::plugin_storage::PluginStorage::default()),
            loaded: OnceCell::new(),
            ready: AtomicBool::new(true),
            bootstrap: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    async fn load(
        &self,
        runtime: &mut polymer_deno::Runtime,
        entrypoint: &str,
    ) -> Result<(), PluginInvocationError> {
        runtime
            .js_runtime_mut()
            .op_state()
            .borrow_mut()
            .put(self.events.clone());
        let state = runtime.js_runtime_mut().op_state();
        state.borrow_mut().put(self.jobs.clone());
        self.loaded
            .get_or_try_init(|| async {
                let result = invoke_optional_export(
                    runtime.js_runtime_mut(),
                    entrypoint,
                    "onPluginLoad",
                    Vec::new(),
                )
                .await;
                match &result {
                    Ok(value) => self.ready.store(
                        !matches!(value, Some(Value::Bool(false))),
                        Ordering::Release,
                    ),
                    Err(_) => self.storage.clear(),
                }
                result?;
                plugin_exports(runtime.js_runtime_mut(), entrypoint).await
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

impl EmulatorLaunchInvocation {
    fn prepare(
        plugin: Arc<dyn EmulatorPluginInstance>,
        arguments: Vec<Value>,
        storage: StorageFactories,
        quick_resume: Option<QuickResumeData>,
        explicit_default: bool,
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
        let topology = serde_json::from_value::<polymer_controllers::ControllerTopology>(
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
            plugin,
            arguments,
            configuration,
            storage,
            quick_resume,
            topology,
            explicit_default,
        })
    }
}

/// Owns plugin scheduling and application-shutdown cancellation.
pub struct PluginRuntime {
    handle: PluginRuntimeHandle,
}

impl PluginRuntime {
    pub fn new(
        registry: PluginRegistry,
        directories: polymer_library::Directories,
        options: PluginRuntimeOptions,
    ) -> Self {
        assert!(
            options.max_concurrent_calls > 0,
            "plugin runtime must allow at least one active call"
        );
        Self {
            handle: PluginRuntimeHandle {
                state: Arc::new(RuntimeState {
                    registry,
                    directories,
                    permits: Arc::new(Semaphore::new(options.max_concurrent_calls)),
                    next_call: AtomicU64::new(1),
                    active: Mutex::new(HashMap::new()),
                    plugins: Mutex::new(HashMap::new()),
                    shutdown: AtomicBool::new(false),
                    jobs: options.jobs,
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

    fn instance(&self, plugin_id: &str) -> Result<Arc<dyn PluginInstance>, PluginInvocationError> {
        self.registry()
            .plugin::<dyn PluginInstance>(plugin_id)
            .ok_or_else(|| {
                PluginInvocationError::new(
                    "NotFoundError",
                    "UNKNOWN_PLUGIN",
                    format!("unknown plugin '{plugin_id}'"),
                )
            })
    }

    /// The plugin's database, with the table holding the user's choices for its options.
    async fn options_database(
        &self,
        plugin_id: &str,
    ) -> Result<turso::Connection, PluginInvocationError> {
        let path = self.directories().plugin_database(plugin_id);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(PluginInvocationError::worker)?;
        }
        let path = path
            .to_str()
            .ok_or_else(|| PluginInvocationError::worker("plugin database path is not UTF-8"))?;
        let connection = turso::Builder::new_local(path)
            .experimental_without_rowid(true)
            .build()
            .await
            .map_err(PluginInvocationError::worker)?
            .connect()
            .map_err(PluginInvocationError::worker)?;
        connection
            .execute(
                "CREATE TABLE IF NOT EXISTS polymer_options (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
                (),
            )
            .await
            .map_err(PluginInvocationError::worker)?;
        Ok(connection)
    }

    /// The user's saved choices for a plugin's options.
    async fn saved_config(&self, plugin_id: &str) -> serde_json::Map<String, Value> {
        let saved = async {
            let connection = self.options_database(plugin_id).await?;
            let mut rows = connection
                .query("SELECT key, value FROM polymer_options", ())
                .await
                .map_err(PluginInvocationError::worker)?;
            let mut saved = serde_json::Map::new();
            while let Some(row) = rows.next().await.map_err(PluginInvocationError::worker)? {
                let key = row
                    .get::<String>(0)
                    .map_err(PluginInvocationError::worker)?;
                let value = row
                    .get::<String>(1)
                    .map_err(PluginInvocationError::worker)?;
                if let Ok(value) = serde_json::from_str(&value) {
                    saved.insert(key, value);
                }
            }
            Ok::<_, PluginInvocationError>(saved)
        };
        saved.await.unwrap_or_else(|error| {
            tracing::warn!(plugin = %plugin_id, %error, "could not read plugin options");
            serde_json::Map::new()
        })
    }

    /// What a fresh worker for the plugin needs, including the user's choices for its options.
    async fn worker_options(
        &self,
        plugin: &dyn PluginInstance,
        storage: StorageFactories,
        quick_resume: Option<QuickResumeData>,
    ) -> Result<WorkerOptions, PluginInvocationError> {
        WorkerOptions::for_plugin(
            plugin,
            self.directories(),
            storage,
            quick_resume,
            &self.saved_config(plugin.id()).await,
            self.registry().jsr_overrides(),
        )
        .map_err(PluginInvocationError::worker)
    }

    /// The current value of each of a plugin's user-facing options.
    pub async fn options(
        &self,
        plugin_id: &str,
    ) -> Result<serde_json::Map<String, Value>, PluginInvocationError> {
        let plugin = self.instance(plugin_id)?;
        let mut effective = plugin
            .config()
            .effective(&self.saved_config(plugin_id).await);
        effective.retain(|key, _| plugin.config().options().any(|(name, _)| name == key));
        Ok(effective)
    }

    /// Saves the user's value for one of a plugin's user-facing options.
    pub async fn set_option(
        &self,
        plugin_id: &str,
        key: &str,
        value: Value,
    ) -> Result<(), PluginInvocationError> {
        self.instance(plugin_id)?
            .config()
            .validate_option(key, &value)
            .map_err(|error| {
                PluginInvocationError::new("TypeError", "INVALID_OPTION", error.to_string())
            })?;
        self.options_database(plugin_id)
            .await?
            .execute(
                "INSERT INTO polymer_options (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                (key, value.to_string()),
            )
            .await
            .map_err(PluginInvocationError::worker)?;
        Ok(())
    }

    fn cache_path(
        &self,
        plugin_id: &str,
        key: &str,
    ) -> Result<std::path::PathBuf, PluginInvocationError> {
        if !self
            .instance(plugin_id)?
            .caches()
            .iter()
            .any(|cache| cache.key == key)
        {
            return Err(PluginInvocationError::new(
                "NotFoundError",
                "UNKNOWN_CACHE",
                format!("plugin '{plugin_id}' has no cache '{key}'"),
            ));
        }
        Ok(self.directories().plugin_cache(plugin_id).join(key))
    }

    /// The bytes used by one of a plugin's caches.
    pub async fn cache_size(
        &self,
        plugin_id: &str,
        key: &str,
    ) -> Result<u64, PluginInvocationError> {
        let path = self.cache_path(plugin_id, key)?;
        tokio::task::spawn_blocking(move || {
            let mut size = 0;
            let mut pending = vec![path];
            while let Some(directory) = pending.pop() {
                let Ok(entries) = std::fs::read_dir(&directory) else {
                    continue;
                };
                for entry in entries.flatten() {
                    match entry.metadata() {
                        Ok(metadata) if metadata.is_dir() => pending.push(entry.path()),
                        Ok(metadata) => size += metadata.len(),
                        Err(_) => {}
                    }
                }
            }
            size
        })
        .await
        .map_err(PluginInvocationError::worker)
    }

    /// Deletes everything in one of a plugin's caches.
    pub async fn clear_cache(
        &self,
        plugin_id: &str,
        key: &str,
    ) -> Result<(), PluginInvocationError> {
        let path = self.cache_path(plugin_id, key)?;
        tokio::task::spawn_blocking(move || match std::fs::remove_dir_all(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => result,
        })
        .await
        .map_err(PluginInvocationError::worker)?
        .map_err(PluginInvocationError::worker)
    }

    pub fn jobs(&self) -> Option<&Arc<polymer_library::jobs::JobStore>> {
        self.state.jobs.as_ref()
    }

    pub fn cancellation_token(&self) -> PluginCancellationToken {
        PluginCancellationToken {
            control: Arc::new(InvocationControl::new()),
        }
    }

    /// Run `onPluginLoad` for every plugin implementing `I` now instead of on its first
    /// invocation. Loads run in the background; failures are reported as load progress.
    pub fn load<I>(&self)
    where
        I: PluginInstance + ?Sized,
        Arc<I>: std::any::Any + Send + Sync,
    {
        for plugin in self.registry().plugins::<I>() {
            let handle = self.clone();
            let id = plugin.id().to_owned();
            tokio::spawn(async move {
                if let Err(error) = handle.exports(&id).await {
                    tracing::warn!(plugin = %id, %error, "plugin failed to load");
                }
            });
        }
    }

    /// The functions a plugin exports, loading it first when nothing has run it yet in this
    /// process.
    pub async fn exports(
        &self,
        plugin_id: &str,
    ) -> Result<BTreeSet<String>, PluginInvocationError> {
        let lifecycle = self.lifecycle(plugin_id);
        if let Some(exports) = lifecycle.loaded.get() {
            return Ok(exports.clone());
        }
        let plugin = self
            .registry()
            .plugin::<dyn PluginInstance>(plugin_id)
            .ok_or_else(|| {
                PluginInvocationError::new(
                    "NotFoundError",
                    "UNKNOWN_PLUGIN",
                    format!("unknown plugin '{plugin_id}'"),
                )
            })?;
        let entrypoint = plugin.entrypoint().to_string();
        let worker = self
            .worker_options(plugin.as_ref(), BTreeMap::new(), None)
            .await?;
        let loading = lifecycle.clone();
        self.run_with_control(self.cancellation_token().control, move |control| {
            run_worker(entrypoint, worker, loading, control, |_| async { Ok(()) })
        })
        .await?;
        Ok(lifecycle.loaded.get().cloned().unwrap_or_default())
    }

    fn lifecycle(&self, plugin_id: &str) -> Arc<PluginLifecycle> {
        self.state
            .plugins
            .lock()
            .entry(plugin_id.into())
            .or_insert_with(|| Arc::new(PluginLifecycle::new(plugin_id, self.state.jobs.clone())))
            .clone()
    }

    fn directories(&self) -> &polymer_library::Directories {
        &self.state.directories
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
        let worker = self
            .worker_options(plugin.as_ref(), BTreeMap::new(), None)
            .await?;
        let lifecycle = self.lifecycle(plugin_id);
        let result = self
            .run_with_control(cancellation.control, move |control| {
                run_worker(
                    entrypoint,
                    worker,
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
        quick_resume: Option<QuickResumeData>,
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

    /// Executes a scraper's async generator in a fresh isolate and validates
    /// every yielded object against the plugin manifest.
    pub async fn scrape(
        &self,
        plugin_id: &str,
        input: ScrapeInput,
        cancellation: PluginCancellationToken,
    ) -> Result<Vec<ScrapeResult>, PluginInvocationError> {
        let plugin = self.scraper(plugin_id)?;
        if !plugin.accepts(&input) {
            return Err(PluginInvocationError::new(
                "TypeError",
                "UNSATISFIED_REQUIREMENTS",
                format!("scraper '{plugin_id}' requirements are not satisfied"),
            ));
        }
        let entrypoint = plugin.entrypoint().to_string();
        let invocation_entrypoint = entrypoint.clone();
        let worker = self
            .worker_options(plugin.as_ref(), BTreeMap::new(), None)
            .await?;
        let lifecycle = self.lifecycle(plugin_id);
        let provided = plugin
            .provides()
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let results = self
            .run_with_control(cancellation.control, move |control| {
                run_worker(
                    entrypoint,
                    worker,
                    lifecycle,
                    control,
                    move |runtime| async move {
                        let mut runtime = runtime;
                        invoke_scrape_generator(
                            runtime.js_runtime_mut(),
                            &invocation_entrypoint,
                            &input,
                            &provided,
                        )
                        .await
                    },
                )
            })
            .await?;
        Ok(results)
    }

    /// Runs a plugin's `onUpdateContents` with `job` as the parent of the jobs it starts.
    pub async fn update_contents(
        &self,
        plugin_id: &str,
        job: Option<Arc<polymer_library::jobs::JobHandle>>,
        cancellation: PluginCancellationToken,
    ) -> Result<(), PluginInvocationError> {
        let plugin = self.instance(plugin_id)?;
        let path = ["onUpdateContents".to_owned()];
        self.invoke_generic(plugin, &path, Vec::new(), cancellation, job)
            .await?;
        self.lifecycle(plugin_id)
            .ready
            .store(true, Ordering::Release);
        Ok(())
    }

    /// Whether a scraper's contents are ready, loading the plugin first when nothing has
    /// run it yet in this process. Contents that are not ready need `onUpdateContents`.
    pub async fn contents_ready(&self, plugin_id: &str) -> Result<bool, PluginInvocationError> {
        self.scraper(plugin_id)?;
        self.exports(plugin_id).await?;
        Ok(self.lifecycle(plugin_id).ready.load(Ordering::Acquire))
    }

    fn scraper(
        &self,
        plugin_id: &str,
    ) -> Result<Arc<dyn ScraperPluginInstance>, PluginInvocationError> {
        self.state
            .registry
            .plugin::<dyn ScraperPluginInstance>(plugin_id)
            .ok_or_else(|| {
                PluginInvocationError::new(
                    "NotFoundError",
                    "UNKNOWN_SCRAPER",
                    format!("unknown scraper '{plugin_id}'"),
                )
            })
    }

    async fn invoke_inner(
        &self,
        plugin_id: &str,
        export_path: &[String],
        arguments: Vec<Value>,
        storage: StorageFactories,
        quick_resume: Option<QuickResumeData>,
        cancellation: PluginCancellationToken,
    ) -> Result<PluginInvocationResult, PluginInvocationError> {
        let plugin = self
            .state
            .registry
            .plugin::<dyn PluginInstance>(plugin_id)
            .ok_or_else(|| {
                PluginInvocationError::new(
                    "NotFoundError",
                    "UNKNOWN_PLUGIN",
                    format!("unknown plugin '{plugin_id}'"),
                )
            })?;
        match ExportInvocation::parse(export_path)? {
            ExportInvocation::EmulatorLaunch { explicit_default } => {
                let emulator = self
                    .state
                    .registry
                    .plugin::<dyn EmulatorPluginInstance>(plugin_id)
                    .ok_or_else(|| {
                        PluginInvocationError::new(
                            "TypeError",
                            "NOT_AN_EMULATOR",
                            format!("plugin '{plugin_id}' is not an emulator"),
                        )
                    })?;
                let invocation = EmulatorLaunchInvocation::prepare(
                    emulator,
                    arguments,
                    storage,
                    quick_resume,
                    explicit_default,
                )?;
                return self.invoke_emulator_launch(invocation, cancellation).await;
            }
            ExportInvocation::Generic => {}
        }
        if plugin.class() == "platform"
            && export_path.last().is_some_and(|name| name == "onBootstrap")
        {
            return Err(PluginInvocationError::new(
                "TypeError",
                "BOOTSTRAP_REQUIRES_DEVICE",
                "Start bootstrap through console storage",
            ));
        }
        self.invoke_generic(plugin, export_path, arguments, cancellation, None)
            .await
    }

    async fn invoke_emulator_launch(
        &self,
        invocation: EmulatorLaunchInvocation,
        cancellation: PluginCancellationToken,
    ) -> Result<PluginInvocationResult, PluginInvocationError> {
        let (mut result, session) = self
            .emulator_launch_with_cancellation(invocation, cancellation)
            .await?;
        let filesystem = result.filesystem.take();
        let retained_session = result.session.take().or(session);
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
        plugin: Arc<dyn PluginInstance>,
        export_path: &[String],
        arguments: Vec<Value>,
        cancellation: PluginCancellationToken,
        job: Option<Arc<polymer_library::jobs::JobHandle>>,
    ) -> Result<PluginInvocationResult, PluginInvocationError> {
        let entrypoint = plugin.entrypoint().to_string();
        let invocation_entrypoint = entrypoint.clone();
        let worker = self
            .worker_options(plugin.as_ref(), BTreeMap::new(), None)
            .await?;

        let lifecycle = self.lifecycle(plugin.id());
        let path = export_path.to_vec();
        let execute = move |control| {
            run_worker(
                entrypoint,
                worker,
                lifecycle,
                control,
                move |runtime| async move {
                    let mut runtime = runtime;
                    invoke_export(
                        runtime.js_runtime_mut(),
                        &invocation_entrypoint,
                        &path,
                        arguments,
                        job,
                    )
                    .await
                },
            )
        };
        let value = self.run_with_control(cancellation.control, execute).await?;
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
    ) -> Result<(EmulatorLaunchArguments, Option<ControllerSession>), PluginInvocationError> {
        let mut worker = self
            .worker_options(
                invocation.plugin.as_ref(),
                invocation.storage.clone(),
                invocation.quick_resume.clone(),
            )
            .await?;
        worker.firmware.platform = invocation.arguments["console"].as_str().map(str::to_owned);
        worker.keys.platform = worker.firmware.platform.clone();
        let lifecycle = self.lifecycle(invocation.plugin.id());
        self.run_with_control(cancellation.control, move |control| {
            ControllerActor::spawn(invocation, worker, lifecycle, control)
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
            let cancelled = control.cancelled_notification();
            tokio::pin!(cancelled);
            if control.is_cancelled() {
                return Err(PluginInvocationError::cancelled());
            }
            tokio::select! {
                permit = self.state.permits.clone().acquire_owned() => {
                    permit.map_err(|_| PluginInvocationError::cancelled())?
                }
                () = &mut cancelled => return Err(PluginInvocationError::cancelled()),
            }
        };
        if control.is_cancelled() {
            return Err(PluginInvocationError::cancelled());
        }
        let worker_control = control.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            worker(worker_control)
        })
        .await
        .map_err(PluginInvocationError::worker)?;
        if control.is_cancelled() {
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
