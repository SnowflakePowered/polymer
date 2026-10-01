use crate::runtime::jobs::PluginJobs;
use deno_core::{OpState, ResourceId, op2};
use deno_error::JsErrorBox;
use polymer_library::{
    StorageManager,
    jobs::{JobKind, JobOutcome, JobProgress, JobSubject},
};
use serde_json::json;
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc};
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct PlatformBinding {
    pub root: PathBuf,
    pub target: Option<(Arc<StorageManager>, String, Uuid)>,
}

impl PlatformBinding {
    fn path(
        &self,
        state: &OpState,
        path: &str,
        cache: Option<&str>,
    ) -> Result<PathBuf, JsErrorBox> {
        if let Some(cache) = cache {
            return state.borrow::<crate::cache::CacheState>().path(cache, path);
        }
        if let Some(relative) = path.strip_prefix("pkg:") {
            let path = self
                .root
                .join(relative)
                .canonicalize()
                .map_err(JsErrorBox::from_err)?;
            if !path.starts_with(&self.root) {
                return Err(JsErrorBox::type_error("Resource escapes plugin package"));
            }
            return Ok(path);
        }
        let path = PathBuf::from(path);
        Ok(if path.is_absolute() {
            path
        } else {
            self.root.join(path)
        })
    }
}

#[op2]
#[serde]
fn op_platform_resource(
    state: &mut OpState,
    #[string] path: String,
) -> Result<serde_json::Value, JsErrorBox> {
    let binding = state.borrow::<PlatformBinding>();
    let path = binding.path(state, &path, None)?;
    match std::fs::metadata(path) {
        Ok(metadata) => {
            Ok(json!({"exists": true, "size": metadata.len(), "directory": metadata.is_dir()}))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({"exists": false})),
        Err(error) => Err(JsErrorBox::from_err(error)),
    }
}

/// The path of a depot the plugin downloaded into one of its caches: a file, or a
/// directory holding every file of the depot.
#[op2]
#[string]
fn op_platform_depot(
    state: &mut OpState,
    #[string] path: String,
    #[string] cache: String,
) -> Result<String, JsErrorBox> {
    let depot = state
        .borrow::<PlatformBinding>()
        .path(state, &path, Some(&cache))?;
    if !depot.exists() {
        return Err(JsErrorBox::type_error("The returned depot does not exist"));
    }
    Ok(depot.to_string_lossy().into_owned())
}

/// Installs a file into the target NAND as an `install` job under the plugin's `parent` scope.
#[op2]
#[serde]
async fn op_platform_install(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
    #[string] cache: Option<String>,
    #[smi] parent: Option<ResourceId>,
) -> Result<polymer_library::ArtifactSummary, JsErrorBox> {
    let (binding, path, jobs, parent) = {
        let state = state.borrow();
        let binding = state.borrow::<PlatformBinding>().clone();
        let path = binding.path(&state, &path, cache.as_deref())?;
        let jobs = state.try_borrow::<PluginJobs>().cloned();
        (binding, path, jobs, PluginJobs::parent(&state, parent))
    };
    let (manager, platform, id) = binding.target.as_ref().ok_or_else(|| {
        JsErrorBox::type_error("NAND installation is only available during host-owned bootstrap")
    })?;
    let subject = JobSubject {
        platform: Some(platform.clone()),
        storage: Some(id.simple().to_string()),
        ..Default::default()
    };
    let title = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let job = match jobs {
        Some(jobs) => jobs.start(JobKind::Install, title, subject, parent).await,
        None => None,
    };
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<JobProgress>();
    let install = manager.install(platform, *id, &path, move |status| {
        let _ = sender.send(JobProgress::from(status));
    });
    let report = async {
        while let Some(progress) = receiver.recv().await {
            if let Some(job) = &job
                && let Err(error) = job.progress(progress).await
            {
                tracing::warn!(%error, job = %job.id(), "could not record install progress");
            }
        }
    };
    let (installed, ()) = tokio::join!(install, report);
    if let Some(job) = job {
        let outcome = installed.as_ref().map(|artifact| {
            JobOutcome::completed(Some(JobProgress {
                detail: Some(json!(artifact)),
                ..Default::default()
            }))
        });
        if let Err(error) = job.settle(outcome).await {
            tracing::warn!(%error, job = %job.id(), "could not finish the install job");
        }
    }
    installed.map_err(JsErrorBox::generic)
}

deno_core::extension!(polymer_platform,
    ops = [op_platform_resource, op_platform_install, op_platform_depot],
    esm_entry_point = "ext:polymer_platform/00_platform.js",
    esm = [dir "platform", "00_platform.js"],
    options = { binding: PlatformBinding },
    state = |state, options| { state.put(options.binding); },
);

pub(crate) fn extension(binding: PlatformBinding) -> deno_core::Extension {
    polymer_platform::init(binding)
}
