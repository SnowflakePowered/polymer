use super::StorageActor;
use actix::{ActorFutureExt, Context, Handler, Message, ResponseActFuture};
use polymer_deno::polymer::consolestorage::{StorageArtifact, StorageError, StorageInspection};
use polymer_library::{
    GameCatalog, StorageKind, StorageManager,
    jobs::{
        JobError, JobHandle, JobKind, JobOutcome, JobProgress, JobRunner, JobSpec, JobStore,
        JobSubject,
    },
};
use polymer_pluginhost::runtime::PluginRuntimeHandle;
use polymer_vfs::ExclusiveResourceKey;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

/// Runs storage jobs one after another. Resumed jobs first discard what their
/// previous run left uncommitted.
#[derive(Message)]
#[rtype(result = "()")]
pub(super) struct RunJobs {
    pub ids: Vec<Uuid>,
    pub resume: bool,
}

impl Handler<RunJobs> for StorageActor {
    type Result = ResponseActFuture<Self, ()>;

    fn handle(&mut self, RunJobs { ids, resume }: RunJobs, _: &mut Context<Self>) -> Self::Result {
        let catalog = self.catalog.clone();
        let run = self.execute(move |manager, plugins| async move {
            for id in ids {
                let run = Self::run_job(
                    manager.clone(),
                    plugins.clone(),
                    catalog.clone(),
                    id,
                    resume,
                );
                if let Err(error) = run.await {
                    tracing::warn!(%error, job = %id, "storage job failed");
                }
            }
            Ok(())
        });
        Box::pin(run.map(|result: Result<(), StorageError>, _, _| {
            if let Err(error) = result {
                tracing::warn!(%error, "could not run storage jobs");
            }
        }))
    }
}

impl JobRunner for super::StorageService {
    fn run(&self, _: &Arc<JobStore>, ids: Vec<Uuid>, resume: bool) {
        self.actor.do_send(RunJobs { ids, resume });
    }
}

impl super::StorageService {
    /// A job that installs the package at `path` into a NAND.
    pub(crate) fn install_spec(platform: &str, storage: Uuid, path: &Path) -> JobSpec {
        let title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self::spec(
            JobKind::Install,
            title,
            json!({ "platform": platform, "storage": storage.simple().to_string(), "path": path }),
        )
    }

    /// A job that runs a platform plugin's bootstrap into a NAND, one at a time per NAND.
    pub(crate) fn bootstrap_spec(platform: &str, storage: Uuid, settings: Value) -> JobSpec {
        JobSpec {
            dedupe: Some(storage.simple().to_string()),
            ..Self::spec(
                JobKind::Bootstrap,
                "Installing system files".to_owned(),
                json!({ "platform": platform, "storage": storage.simple().to_string(), "settings": settings }),
            )
        }
    }

    pub(super) fn spec(kind: JobKind, title: String, params: Value) -> JobSpec {
        JobSpec {
            kind,
            parent: None,
            title,
            subject: JobSubject {
                platform: params["platform"].as_str().map(str::to_owned),
                storage: params["storage"].as_str().map(str::to_owned),
                ..Default::default()
            },
            params,
            dedupe: None,
            steps: Vec::new(),
            persistent: true,
        }
    }
}

impl StorageActor {
    async fn run_job(
        manager: Arc<StorageManager>,
        plugins: PluginRuntimeHandle,
        catalog: Option<Arc<GameCatalog>>,
        id: Uuid,
        resume: bool,
    ) -> Result<(), StorageError> {
        let jobs = Self::jobs(&plugins)?;
        let Some(job) = jobs.claim(id).await.map_err(StorageError::from_display)? else {
            return Ok(());
        };
        let job = Arc::new(job);
        match job.job().await.map_err(StorageError::from_display)?.kind {
            JobKind::Install => Self::install_job(manager, job, resume).await.map(drop),
            JobKind::Bootstrap => Self::bootstrap_job(manager, plugins, job, resume)
                .await
                .map(drop),
            JobKind::Check => Self::check_job(manager, plugins, catalog, job)
                .await
                .map(drop),
            JobKind::Update => Self::update_job(manager, plugins, job).await,
            kind => Err(StorageError::from_display(JobError::Unsupported(kind))),
        }
    }

    pub(super) fn jobs(plugins: &PluginRuntimeHandle) -> Result<Arc<JobStore>, StorageError> {
        plugins
            .jobs()
            .cloned()
            .ok_or_else(|| StorageError::from("jobs are unavailable"))
    }

    /// Installs a package into a NAND. A resumed install first discards what the
    /// interrupted one left uncommitted, then installs from the source again.
    pub(super) async fn install_job(
        manager: Arc<StorageManager>,
        job: Arc<JobHandle>,
        resume: bool,
    ) -> Result<StorageArtifact, StorageError> {
        let result = async {
            let params = job.job().await.map_err(StorageError::from_display)?.params;
            let (platform, id) = Self::target(&params)?;
            let path = PathBuf::from(params["path"].as_str().unwrap_or_default());
            let _lease = ExclusiveResourceKey::Resource(id)
                .acquire()
                .map_err(StorageError::from_display)?;
            if resume {
                Self::restore(&manager, &platform, id).await?;
            }
            let report = job.reporter();
            report(JobProgress {
                message: Some("Waiting to install".to_owned()),
                ..Default::default()
            });
            manager
                .install(&platform, id, &path, move |status| report(status.into()))
                .await
                .map_err(StorageError::from)
        }
        .await;
        let outcome = result.as_ref().map(|artifact| {
            JobOutcome::completed(Some(JobProgress {
                title: Some(artifact.label.clone()),
                icon: artifact.icons.first().cloned(),
                message: Some("Installed".to_owned()),
                detail: Some(json!(artifact)),
                ..Default::default()
            }))
        });
        Self::finish(&job, outcome).await;
        result
    }

    /// Runs a platform plugin's bootstrap into a NAND, restoring it first when resuming.
    pub(super) async fn bootstrap_job(
        manager: Arc<StorageManager>,
        plugins: PluginRuntimeHandle,
        job: Arc<JobHandle>,
        resume: bool,
    ) -> Result<StorageInspection, StorageError> {
        let result = async {
            let params = job.job().await.map_err(StorageError::from_display)?.params;
            let (platform, id) = Self::target(&params)?;
            let _lease = ExclusiveResourceKey::Resource(id)
                .acquire()
                .map_err(StorageError::from_display)?;
            if resume {
                Self::restore(&manager, &platform, id).await?;
            }
            plugins
                .bootstrap(
                    manager,
                    &platform,
                    id,
                    params["settings"].clone(),
                    plugins.cancellation_token(),
                    Some(job.clone()),
                )
                .await
                .map_err(StorageError::from_display)
        }
        .await;
        let outcome = result.as_ref().map(|_| {
            JobOutcome::completed(Some(JobProgress {
                message: Some("System setup complete".to_owned()),
                ..Default::default()
            }))
        });
        Self::finish(&job, outcome).await;
        result
    }

    pub(super) async fn finish(job: &JobHandle, outcome: Result<JobOutcome, &StorageError>) {
        if let Err(error) = job.settle(outcome).await {
            tracing::warn!(%error, job = %job.id(), "could not finish storage job");
        }
    }

    pub(super) fn target(params: &Value) -> Result<(String, Uuid), StorageError> {
        let platform = params["platform"]
            .as_str()
            .ok_or("storage job has no platform")?;
        let id = Self::parse(params["storage"].as_str().unwrap_or_default())?;
        Ok((platform.to_owned(), id))
    }

    pub(super) async fn restore(
        manager: &StorageManager,
        platform: &str,
        id: Uuid,
    ) -> Result<(), StorageError> {
        manager
            .open(StorageKind::Nand, platform, id)
            .await
            .map_err(StorageError::from_display)?
            .as_nand()
            .ok_or("device is not a NAND")?
            .recover()
            .map_err(StorageError::from_display)
    }
}
