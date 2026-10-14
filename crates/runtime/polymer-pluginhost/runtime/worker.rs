use super::js::ExtensionFunction;
use super::{InvocationControl, PluginInvocationError, PluginLifecycle};
use crate::{
    emulator::QuickResumeData,
    filesystem::StorageFactories,
    loading::{PackageError, PluginModuleLoader},
    plugin::PluginInstance,
    scraper::{ScrapeInput, ScrapeResult},
};
use deno_core::{serde_v8, v8};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeSet, future::Future, rc::Rc, sync::Arc};

const MAX_SCRAPE_RESULTS: usize = 4096;

pub(super) struct WorkerOptions {
    pub(super) firmware: crate::firmware::FirmwareBinding,
    pub(super) keys: crate::keys::KeyBinding,
    loader: PluginModuleLoader,
    filesystem: crate::filesystem::FilesystemOptions,
    cache: crate::cache::CacheOptions,
    database: crate::database::DatabaseOptions,
    config: serde_json::Map<String, Value>,
    manifest: Value,
    pub(super) platform: crate::platform::PlatformBinding,
}

impl WorkerOptions {
    pub(super) fn for_plugin(
        plugin: &dyn PluginInstance,
        directories: &polymer_library::Directories,
        storage: StorageFactories,
        quick_resume: Option<QuickResumeData>,
        saved: &serde_json::Map<String, Value>,
        jsr_overrides: Arc<dashmap::DashMap<String, std::path::PathBuf>>,
    ) -> Result<Self, PackageError> {
        let mut filesystem =
            crate::filesystem::FilesystemOptions::new(plugin.module_loader().root().to_owned());
        filesystem.scratch_root = Some(directories.plugin_scratch(plugin.id()));
        filesystem.storage = storage;
        filesystem.quick_resume = quick_resume;
        let cache = crate::cache::CacheOptions::for_plugin(directories, plugin);
        filesystem.caches = cache.filesystem_caches();
        Ok(Self {
            firmware: crate::firmware::FirmwareBinding::new(directories.firmware()),
            keys: crate::keys::KeyBinding::new(directories.keys()),
            platform: crate::platform::PlatformBinding {
                root: plugin.module_loader().root().to_owned(),
                target: None,
            },
            loader: plugin
                .module_loader()
                .with_directories(directories)
                .with_jsr_overrides(jsr_overrides),
            filesystem,
            cache,
            database: crate::database::DatabaseOptions::for_plugin(directories, plugin.id()),
            config: plugin.config().effective(saved),
            manifest: plugin.manifest().clone(),
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
    F: FnOnce(polymer_deno::Runtime) -> Fut,
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
        tokio::select! {
            () = control.cancelled_notification() => Err(PluginInvocationError::cancelled()),
            result = async {
                lifecycle.load(&mut runtime, &entrypoint).await?;
                operation(runtime).await
            } => result,
        }
    })
}

pub(super) fn create_worker_runtime(
    entrypoint: &str,
    options: WorkerOptions,
    storage: Arc<crate::plugin_storage::PluginStorage>,
) -> Result<polymer_deno::Runtime, PluginInvocationError> {
    let WorkerOptions {
        firmware,
        keys,
        loader,
        filesystem,
        cache,
        database,
        config,
        manifest,
        platform,
    } = options;
    let main_module =
        deno_core::ModuleSpecifier::parse(entrypoint).map_err(PluginInvocationError::worker)?;
    let network_hosts = loader.network_hosts().to_vec();
    let insecure_hosts = loader.insecure_hosts().to_vec();
    let download = crate::download::DownloadBinding {
        insecure_hosts: loader.insecure_hosts().clone(),
    };
    let mut runtime = polymer_deno::Runtime::new(polymer_deno::RuntimeOptions {
        main_module,
        module_loader: Rc::new(loader),
        extensions: vec![
            crate::firmware::extension(firmware),
            crate::keys::extension(keys),
            crate::digest::extension(),
            crate::actions::extension(),
            crate::automation::extension(),
            crate::controller::extension(),
            crate::configuration::extension(),
            crate::emulator::js::extension(),
            crate::cache::extension(cache),
            crate::download::extension(download),
            crate::unarchive::extension(),
            crate::platform::extension(platform),
            crate::database::extension(database),
            crate::filesystem::extension(filesystem),
            crate::plugin_storage::extension(storage),
            crate::plugin_context::extension(config, manifest),
            crate::runtime::js::extension(),
            crate::scraper::js::extension(),
        ],
        user_agent: format!("PolymerPluginHost/{}", env!("CARGO_PKG_VERSION")),
        local_storage: None,
        inspect: false,
        insecure_hosts,
    });
    runtime
        .restrict_network(&network_hosts)
        .map_err(PluginInvocationError::worker)?;
    Ok(runtime)
}

pub(super) async fn invoke_scrape_generator(
    runtime: &mut deno_core::JsRuntime,
    entrypoint: &str,
    input: &ScrapeInput,
    provided: &BTreeSet<String>,
) -> Result<Vec<ScrapeResult>, PluginInvocationError> {
    let mut input = input.clone();
    for package in &mut input.packages {
        runtime
            .op_state()
            .borrow_mut()
            .borrow_mut::<crate::digest::DigestState>()
            .package(package);
    }
    let initialize = ExtensionFunction::get(runtime, "__polymerScraper", "initialize")
        .map_err(PluginInvocationError::worker)?;
    let arguments = [
        ExtensionFunction::argument(runtime, entrypoint).map_err(PluginInvocationError::worker)?,
        ExtensionFunction::argument(runtime, input).map_err(PluginInvocationError::worker)?,
    ];
    initialize
        .call(runtime, &arguments)
        .await
        .map_err(PluginInvocationError::worker)?;

    #[derive(serde::Deserialize)]
    struct GeneratorStep {
        done: bool,
        #[serde(default)]
        value: Option<ScrapeResult>,
    }

    let next = ExtensionFunction::get(runtime, "__polymerScraper", "next")
        .map_err(PluginInvocationError::worker)?;
    let mut results = Vec::new();
    loop {
        let resolved = next
            .call(runtime, &[])
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
        let result = step.value.ok_or_else(|| {
            PluginInvocationError::new(
                "TypeError",
                "INVALID_SCRAPE_RESULT",
                "onScrape yielded no result value",
            )
        })?;
        result.validate(provided)?;
        results.push(result);
    }
    Ok(results)
}

pub(super) async fn invoke_export(
    runtime: &mut deno_core::JsRuntime,
    entrypoint: &str,
    export_path: &[String],
    mut arguments: Vec<Value>,
    job: Option<Arc<polymer_library::jobs::JobHandle>>,
) -> Result<Value, PluginInvocationError> {
    let invoke = ExtensionFunction::get(runtime, "__polymerPluginRuntime", "invoke")
        .map_err(PluginInvocationError::worker)?;
    let scope = {
        let state = runtime.op_state();
        let mut state = state.borrow_mut();
        state
            .borrow_mut::<crate::digest::DigestState>()
            .arguments(&mut arguments);
        job.map(|job| super::jobs::PluginJobs::register(&mut state, job))
    };
    let values = [
        ExtensionFunction::argument(runtime, entrypoint).map_err(PluginInvocationError::worker)?,
        ExtensionFunction::argument(runtime, export_path).map_err(PluginInvocationError::worker)?,
        ExtensionFunction::argument(runtime, arguments).map_err(PluginInvocationError::worker)?,
        ExtensionFunction::argument(runtime, scope).map_err(PluginInvocationError::worker)?,
    ];
    let resolved = invoke
        .call(runtime, &values)
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
    mut arguments: Vec<Value>,
) -> Result<Option<Value>, PluginInvocationError> {
    let invoke = ExtensionFunction::get(runtime, "__polymerPluginRuntime", "invokeOptional")
        .map_err(PluginInvocationError::worker)?;
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<crate::digest::DigestState>()
        .arguments(&mut arguments);
    let values = [
        ExtensionFunction::argument(runtime, entrypoint).map_err(PluginInvocationError::worker)?,
        ExtensionFunction::argument(runtime, name).map_err(PluginInvocationError::worker)?,
        ExtensionFunction::argument(runtime, arguments).map_err(PluginInvocationError::worker)?,
    ];
    let resolved = invoke
        .call(runtime, &values)
        .await
        .map_err(PluginInvocationError::worker)?;
    deno_core::scope!(scope, runtime);
    let value = v8::Local::new(scope, resolved);
    let value: OptionalExportResult =
        serde_v8::from_v8(scope, value).map_err(PluginInvocationError::worker)?;
    Ok(value.found.then_some(value.value))
}

/// The names of the functions the plugin exports, directly or on its default export.
pub(super) async fn plugin_exports(
    runtime: &mut deno_core::JsRuntime,
    entrypoint: &str,
) -> Result<BTreeSet<String>, PluginInvocationError> {
    let exports = ExtensionFunction::get(runtime, "__polymerPluginRuntime", "exports")
        .map_err(PluginInvocationError::worker)?;
    let values =
        [ExtensionFunction::argument(runtime, entrypoint)
            .map_err(PluginInvocationError::worker)?];
    let resolved = exports
        .call(runtime, &values)
        .await
        .map_err(PluginInvocationError::worker)?;
    deno_core::scope!(scope, runtime);
    let value = v8::Local::new(scope, resolved);
    serde_v8::from_v8(scope, value).map_err(PluginInvocationError::worker)
}

#[derive(Deserialize)]
struct OptionalExportResult {
    found: bool,
    value: Value,
}
