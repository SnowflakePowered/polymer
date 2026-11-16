use crate::{cache::CacheState, runtime::jobs::PluginJobs};
use ::polymer_unarchive::{Archive, ExtractOptions};
use deno_core::{OpState, ResourceId, op2};
use deno_error::JsErrorBox;
use polymer_library::jobs::{JobKind, JobOutcome, JobProgress, JobSubject};
use std::{cell::RefCell, rc::Rc};

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnarchiveOptions {
    #[serde(default)]
    strip_root: bool,
}

/// Extract an archive in one of the plugin's caches into a cache directory, as a job.
#[op2]
async fn op_unarchive(
    state: Rc<RefCell<OpState>>,
    #[string] archive_cache: String,
    #[string] archive: String,
    #[string] destination_cache: String,
    #[string] destination: String,
    #[serde] options: Option<UnarchiveOptions>,
    #[smi] parent: Option<ResourceId>,
) -> Result<(), JsErrorBox> {
    let (archive, destination, jobs, parent) = {
        let state = state.borrow();
        let caches = state.borrow::<CacheState>();
        (
            caches.path(&archive_cache, &archive)?,
            caches.path(&destination_cache, &destination)?,
            state.try_borrow::<PluginJobs>().cloned(),
            PluginJobs::parent(&state, parent),
        )
    };
    let title = archive
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let job = match jobs {
        Some(jobs) => {
            jobs.start(JobKind::Extract, title, JobSubject::default(), parent)
                .await
        }
        None => None,
    };
    let reporter = job.as_ref().map(|job| job.reporter());
    let options = ExtractOptions {
        strip_root: options.unwrap_or_default().strip_root,
    };
    let result = tokio::task::spawn_blocking(move || {
        Archive::open(archive)?.extract(&destination, options, |progress| {
            if let Some(reporter) = &reporter {
                reporter(JobProgress {
                    completed: Some(progress.completed),
                    total: Some(progress.total),
                    ..Default::default()
                });
            }
        })
    })
    .await
    .map_err(|error| JsErrorBox::generic(error.to_string()))?
    .map_err(|error| JsErrorBox::generic(error.to_string()));
    if let Some(job) = job
        && let Err(error) = job
            .settle(
                result
                    .as_ref()
                    .map(|()| JobOutcome::completed(None))
                    .map_err(|error| error.to_string()),
            )
            .await
    {
        tracing::warn!(%error, job = %job.id(), "could not settle extraction job");
    }
    result
}

deno_core::extension!(polymer_unarchive,
    deps = [polymer_cache],
    ops = [op_unarchive],
    esm_entry_point = "ext:polymer_unarchive/00_unarchive.js",
    esm = [dir "unarchive/js", "00_unarchive.js"],
);

pub(crate) fn extension() -> deno_core::Extension {
    polymer_unarchive::init()
}
