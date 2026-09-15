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
    loader: PluginModuleLoader,
    filesystem: crate::filesystem::FilesystemOptions,
    cache: crate::cache::CacheOptions,
    database: crate::database::DatabaseOptions,
    pub(super) platform: crate::platform::PlatformBinding,
}

impl WorkerOptions {
    pub(super) fn for_plugin(
        plugin: &dyn PluginInstance,
        directories: &polymer_library::Directories,
        storage: StorageFactories,
        quick_resume: Option<QuickResumeData>,
    ) -> Result<Self, PackageError> {
        let mut filesystem =
            crate::filesystem::FilesystemOptions::new(plugin.module_loader().root().to_owned());
        filesystem.scratch_root = Some(directories.plugin_scratch(plugin.id()));
        filesystem.storage = storage;
        filesystem.quick_resume = quick_resume;
        let cache = crate::cache::CacheOptions::for_plugin(directories, plugin);
        filesystem.caches = cache.filesystem_caches();
        Ok(Self {
            platform: crate::platform::PlatformBinding {
                manager: Arc::new(polymer_library::StorageManager::new(
                    directories.consolestorage(),
                )),
                root: plugin.module_loader().root().to_owned(),
                device: None,
            },
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
        loader,
        filesystem,
        cache,
        database,
        platform,
    } = options;
    let main_module =
        deno_core::ModuleSpecifier::parse(entrypoint).map_err(PluginInvocationError::worker)?;
    let network_hosts = loader.network_hosts().to_vec();
    let mut runtime = polymer_deno::Runtime::new(polymer_deno::RuntimeOptions {
        main_module,
        module_loader: Rc::new(loader),
        extensions: vec![
            crate::digest::extension(),
            crate::actions::extension(),
            crate::automation::extension(),
            crate::controller::extension(),
            crate::configuration::extension(),
            crate::emulator::js::extension(),
            crate::cache::extension(cache),
            crate::platform::extension(platform),
            crate::database::extension(database),
            crate::filesystem::extension(filesystem),
            crate::plugin_storage::extension(storage),
            crate::runtime::js::extension(),
            crate::scraper::js::extension(),
        ],
        user_agent: format!("PolymerPluginHost/{}", env!("CARGO_PKG_VERSION")),
        local_storage: None,
        inspect: false,
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
) -> Result<Value, PluginInvocationError> {
    let progress = matches!(export_path, [name] if name == "updateContents")
        || matches!(export_path, [default, name] if default == "default" && name == "updateContents");
    let progress = progress.then(|| {
        runtime.op_state().borrow().borrow::<Arc<super::events::EventEmitter>>().scope(
            "loadprogress",
            serde_json::json!({"state": "loading", "message": "Updating plugin contents"}),
            serde_json::json!({"state": "failed", "message": "Plugin contents update interrupted"}),
        )
    });
    let invoke = ExtensionFunction::get(runtime, "__polymerPluginRuntime", "invoke")
        .map_err(PluginInvocationError::worker)?;
    runtime
        .op_state()
        .borrow_mut()
        .borrow_mut::<crate::digest::DigestState>()
        .arguments(&mut arguments);
    let values = [
        ExtensionFunction::argument(runtime, entrypoint).map_err(PluginInvocationError::worker)?,
        ExtensionFunction::argument(runtime, export_path).map_err(PluginInvocationError::worker)?,
        ExtensionFunction::argument(runtime, arguments).map_err(PluginInvocationError::worker)?,
    ];
    let resolved = invoke
        .call(runtime, &values)
        .await
        .map_err(PluginInvocationError::worker);
    if let Some(progress) = progress {
        progress.finish(match &resolved {
            Ok(_) => serde_json::json!({"state": "ready", "message": "Plugin contents updated"}),
            Err(error) => serde_json::json!({"state": "failed", "message": error.to_string()}),
        });
    }
    let resolved = resolved?;
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

#[derive(Deserialize)]
struct OptionalExportResult {
    found: bool,
    value: Value,
}
