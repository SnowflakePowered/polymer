use super::StorageActor;
use polymer_deno::polymer::consolestorage::{Platforms, StorageError};
use polymer_library::{
    GameCatalog, Launchable, LaunchableUri, StorageKind, StorageManager,
    jobs::{JobHandle, JobKind, JobOutcome, JobProgress, JobSpec, JobStore, JobSubject},
};
use polymer_pluginhost::runtime::{PluginRuntimeHandle, UpdateTarget};
use serde_json::{Value, json};
use std::sync::Arc;

impl super::StorageService {
    /// An in-memory job that asks the platform plugin which updates a library title has.
    pub(crate) fn check_spec(launchable: &Launchable) -> Result<JobSpec, StorageError> {
        let title_id = Self::title_id(launchable)?;
        let artifact = match &launchable.identified.path {
            LaunchableUri::Artifact(artifact) => Some(artifact.artifact.clone()),
            LaunchableUri::File(_) => None,
        };
        let title = json!({ "titleId": title_id, "version": null, "artifactId": artifact });
        let spec = Self::title_spec(
            launchable,
            JobKind::Check,
            format!("Checking {title_id} for updates"),
            json!({ "title": title }),
        );
        Ok(JobSpec {
            dedupe: Some(launchable.id.simple().to_string()),
            persistent: false,
            ..spec
        })
    }

    /// A persistent job that downloads one update candidate for a library title and installs it.
    pub(crate) fn update_spec(
        launchable: &Launchable,
        candidate: Value,
    ) -> Result<JobSpec, StorageError> {
        let title_id = Self::title_id(launchable)?;
        let label = candidate["label"].as_str().unwrap_or(title_id).to_owned();
        let dedupe = format!(
            "{}:{title_id}:{}",
            launchable.identified.platform,
            candidate["version"].as_str().unwrap_or_default()
        );
        let spec = Self::title_spec(
            launchable,
            JobKind::Update,
            label,
            json!({ "candidate": candidate }),
        );
        Ok(JobSpec {
            dedupe: Some(dedupe),
            ..spec
        })
    }

    /// A storage job for a library title. An installed title targets its own NAND; any other
    /// title targets the NAND chosen when the job runs.
    fn title_spec(
        launchable: &Launchable,
        kind: JobKind,
        title: String,
        mut params: Value,
    ) -> JobSpec {
        let storage = match &launchable.identified.path {
            LaunchableUri::Artifact(artifact) => Some(artifact.storage.simple().to_string()),
            LaunchableUri::File(_) => None,
        };
        params["platform"] = json!(launchable.identified.platform);
        params["storage"] = json!(storage);
        params["launchable"] = json!(launchable.id.simple().to_string());
        let spec = Self::spec(kind, title, params);
        JobSpec {
            subject: JobSubject {
                binary_id: Some(launchable.binary_id.simple().to_string()),
                ..spec.subject
            },
            ..spec
        }
    }

    /// Queues a `check` job for each title on a platform whose plugin provides updates.
    /// Titles without a title ID are skipped.
    pub(crate) async fn check_titles(
        &self,
        store: &Arc<JobStore>,
        launchables: impl IntoIterator<Item = Launchable>,
    ) -> Result<(), StorageError> {
        let platforms = self
            .actor
            .send(Platforms)
            .await
            .map_err(StorageError::from_display)??;
        for launchable in launchables {
            let updatable = platforms.iter().any(|platform| {
                platform.updates && platform.platform == launchable.identified.platform
            });
            if !updatable || launchable.identified.title_id.is_none() {
                continue;
            }
            store
                .submit(Self::check_spec(&launchable)?)
                .await
                .map_err(StorageError::from_display)?;
        }
        Ok(())
    }

    fn title_id(launchable: &Launchable) -> Result<&str, StorageError> {
        launchable.identified.title_id.as_deref().ok_or_else(|| {
            StorageError("the title has no title ID to check for updates".to_owned())
        })
    }
}

impl StorageActor {
    /// Runs the platform plugin's check for one title and records its candidates in the
    /// job and in the library's cache of the title's updates. The check sees the updates
    /// installed in the title's NAND, or in the platform's default NAND for a title that is
    /// not installed; without a NAND, nothing counts as installed.
    pub(super) async fn check_job(
        manager: Arc<StorageManager>,
        plugins: PluginRuntimeHandle,
        catalog: Option<Arc<GameCatalog>>,
        job: Arc<JobHandle>,
    ) -> Result<Value, StorageError> {
        let result = async {
            let params = job.job().await.map_err(StorageError::from_display)?.params;
            let platform = Self::platform(&params)?;
            let storage = match params["storage"].as_str() {
                Some(storage) => Some(Self::parse(storage)?),
                None => manager
                    .oldest(StorageKind::Nand, &platform)
                    .map_err(StorageError::from_display)?,
            };
            let target = UpdateTarget {
                manager,
                platform,
                storage,
            };
            let candidates = plugins
                .check_updates(
                    target,
                    params["title"].clone(),
                    plugins.cancellation_token(),
                    Some(job.clone()),
                )
                .await
                .map_err(StorageError::from_display)?;
            if let (Some(catalog), Some(launchable)) = (&catalog, params["launchable"].as_str()) {
                let checked = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;
                catalog
                    .replace_updates(
                        Self::parse(launchable)?,
                        candidates.as_array().map_or(&[], Vec::as_slice),
                        checked,
                    )
                    .await
                    .map_err(StorageError::from_display)?;
            }
            Ok(candidates)
        }
        .await;
        let outcome = result.as_ref().map(|candidates| {
            let found = candidates.as_array().map_or(0, Vec::len);
            JobOutcome::completed(Some(JobProgress {
                message: Some(match found {
                    0 => "Up to date".to_owned(),
                    1 => "1 update available".to_owned(),
                    found => format!("{found} updates available"),
                }),
                detail: Some(json!({ "candidates": candidates })),
                ..Default::default()
            }))
        });
        Self::finish(&job, outcome).await;
        result
    }

    /// Downloads one update candidate as a depot, a file or a directory, recorded as
    /// `detail.depot`, then queues a child `install` job that installs it like any other
    /// depot, recorded as `detail.install`. A title that is not installed gets the platform's default NAND,
    /// created and bootstrapped when there is none. A resumed job whose install is already
    /// queued does not download again.
    pub(super) async fn update_job(
        manager: Arc<StorageManager>,
        plugins: PluginRuntimeHandle,
        job: Arc<JobHandle>,
    ) -> Result<(), StorageError> {
        let result = async {
            let current = job.job().await.map_err(StorageError::from_display)?;
            if current.detail["install"].is_string() {
                return Ok(());
            }
            let mut params = current.params;
            let platform = Self::platform(&params)?;
            let depot = plugins
                .fetch_update(
                    &manager,
                    &platform,
                    params["candidate"].clone(),
                    plugins.cancellation_token(),
                    Some(job.clone()),
                )
                .await
                .map_err(StorageError::from_display)?;
            let storage = match params["storage"].as_str() {
                Some(storage) => Self::parse(storage)?,
                None => {
                    let storage =
                        Self::provide_nand(&manager, &plugins, &platform, true, Some(job.id()))
                            .await?;
                    params["storage"] = json!(storage.simple().to_string());
                    job.reparameterize(params)
                        .await
                        .map_err(StorageError::from_display)?;
                    storage
                }
            };
            let jobs = Self::jobs(&plugins)?;
            let spec = JobSpec {
                parent: Some(job.id()),
                ..super::StorageService::install_spec(&platform, storage, &depot)
            };
            let (install, _) = jobs
                .enqueue(spec)
                .await
                .map_err(StorageError::from_display)?;
            job.record(json!({ "depot": depot, "install": install.id.simple().to_string() }))
                .await
                .map_err(StorageError::from_display)?;
            jobs.dispatch(&install).map_err(StorageError::from_display)
        }
        .await;
        let outcome = result.as_ref().map(|()| {
            JobOutcome::completed(Some(JobProgress {
                message: Some("Downloaded".to_owned()),
                ..Default::default()
            }))
        });
        Self::finish(&job, outcome).await;
        result
    }

    fn platform(params: &Value) -> Result<String, StorageError> {
        params["platform"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| StorageError("the job has no platform".to_owned()))
    }
}
