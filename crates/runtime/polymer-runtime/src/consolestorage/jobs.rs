use super::StorageActor;
use actix::{Context, Handler, Message, ResponseActFuture};
use polymer_deno::polymer::consolestorage::{StorageArtifact, StorageError, StorageInspection};
use polymer_library::{
    StorageKind, StorageManager,
    jobs::{JobHandle, JobKind, JobProgress, JobSpec, JobState, JobStore, JobSubject},
};
use polymer_pluginhost::runtime::PluginRuntimeHandle;
use polymer_vfs::ExclusiveResourceKey;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;

/// Resumes install and bootstrap jobs that a previous process left unfinished.
#[derive(Message)]
#[rtype(result = "Result<(), StorageError>")]
pub(super) struct ResumeJobs;

/// Runs a queued install or bootstrap job.
#[derive(Message)]
#[rtype(result = "Result<(), StorageError>")]
pub(super) struct RunJob(pub Uuid);

impl Handler<ResumeJobs> for StorageActor {
    type Result = ResponseActFuture<Self, Result<(), StorageError>>;

    fn handle(&mut self, _: ResumeJobs, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |manager, plugins| async move {
            let jobs = Self::jobs(&plugins)?;
            for kind in [JobKind::Install, JobKind::Bootstrap] {
                for job in jobs
                    .recover(kind)
                    .await
                    .map_err(StorageError::from_display)?
                {
                    let run = Self::run_job(manager.clone(), plugins.clone(), job.id, true);
                    if let Err(error) = run.await {
                        tracing::warn!(%error, job = %job.id, "resumed storage job failed");
                    }
                }
            }
            Ok(())
        })
    }
}

impl Handler<RunJob> for StorageActor {
    type Result = ResponseActFuture<Self, Result<(), StorageError>>;

    fn handle(&mut self, RunJob(id): RunJob, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |manager, plugins| Self::run_job(manager, plugins, id, false))
    }
}

impl StorageActor {
    async fn run_job(
        manager: Arc<StorageManager>,
        plugins: PluginRuntimeHandle,
        id: Uuid,
        resume: bool,
    ) -> Result<(), StorageError> {
        let jobs = Self::jobs(&plugins)?;
        let Some(job) = jobs.claim(id).await.map_err(StorageError::from_display)? else {
            return Ok(());
        };
        let job = Arc::new(job);
        match job.job().await.map_err(StorageError::from_display)?.kind {
            JobKind::Bootstrap => Self::bootstrap_job(manager, plugins, job, resume)
                .await
                .map(drop),
            _ => Self::install_job(manager, job, resume).await.map(drop),
        }
    }

    pub(super) fn jobs(plugins: &PluginRuntimeHandle) -> Result<Arc<JobStore>, StorageError> {
        plugins
            .jobs()
            .cloned()
            .ok_or_else(|| StorageError::from("jobs are unavailable"))
    }

    pub(super) fn job_spec(kind: JobKind, title: String, params: Value) -> JobSpec {
        JobSpec {
            kind,
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
        let completed = result.as_ref().ok().map(|artifact| JobProgress {
            title: Some(artifact.label.clone()),
            icon: artifact.icons.first().cloned(),
            message: Some("Installed".to_owned()),
            detail: Some(json!(artifact)),
            ..Default::default()
        });
        Self::finish(&job, &result, completed).await;
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
        let completed = result.is_ok().then(|| JobProgress {
            message: Some("System setup complete".to_owned()),
            ..Default::default()
        });
        Self::finish(&job, &result, completed).await;
        result
    }

    async fn finish<T>(
        job: &JobHandle,
        result: &Result<T, StorageError>,
        completed: Option<JobProgress>,
    ) {
        if let Some(progress) = completed {
            let _ = job.progress(progress).await;
        }
        let (state, error) = match result {
            Ok(_) => (JobState::Completed, None),
            Err(error) => (JobState::Failed, Some(error.to_string())),
        };
        if let Err(error) = job.finish(state, error).await {
            tracing::warn!(%error, job = %job.id(), "could not finish storage job");
        }
    }

    fn target(params: &Value) -> Result<(String, Uuid), StorageError> {
        let platform = params["platform"]
            .as_str()
            .ok_or("storage job has no platform")?;
        let id = Self::parse(params["storage"].as_str().unwrap_or_default())?;
        Ok((platform.to_owned(), id))
    }

    async fn restore(
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
