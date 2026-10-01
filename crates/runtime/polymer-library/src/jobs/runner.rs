use std::{fmt::Display, sync::Arc};
use uuid::Uuid;

use super::{Job, JobError, JobHandle, JobKind, JobProgress, JobSpec, JobState, JobStore};

/// Runs the jobs of the kinds it is registered for.
pub trait JobRunner: Send + Sync + 'static {
    /// Starts the queued jobs `ids` on the runner's own executor, in order. `resume` is set
    /// for jobs that ran before, either in a previous process or as a retried attempt.
    fn run(&self, store: &Arc<JobStore>, ids: Vec<Uuid>, resume: bool);
}

/// How a job ended when its work succeeded.
pub struct JobOutcome {
    pub state: JobState,
    pub progress: Option<JobProgress>,
}

impl JobOutcome {
    pub fn completed(progress: Option<JobProgress>) -> Self {
        Self {
            state: JobState::Completed,
            progress,
        }
    }
}

impl JobStore {
    /// Hands jobs of `kinds` to `runner` from now on.
    pub fn register(&self, kinds: &[JobKind], runner: Arc<dyn JobRunner>) {
        for kind in kinds {
            self.runners.insert(*kind, runner.clone());
        }
    }

    /// Hands the jobs of `kinds` that a previous process left unfinished to their runner,
    /// recovered in the order of `kinds`, which share one runner.
    pub async fn resume(self: &Arc<Self>, kinds: &[JobKind]) -> Result<(), JobError> {
        let Some(kind) = kinds.first() else {
            return Ok(());
        };
        let runner = self.runner(*kind)?;
        let mut interrupted = Vec::new();
        for kind in kinds {
            interrupted.extend(self.recover(*kind).await?.into_iter().map(|job| job.id));
        }
        if !interrupted.is_empty() {
            runner.run(self, interrupted, true);
        }
        Ok(())
    }

    /// Fails jobs of `kind` that a previous process left unfinished, for kinds that only
    /// whoever needs their work starts, such as downloads.
    pub async fn abandon(&self, kind: JobKind) -> Result<(), JobError> {
        for job in self.recover(kind).await? {
            self.modify(job.id, |job| {
                job.state = JobState::Failed;
                job.error = Some("Interrupted".to_owned());
            })
            .await?;
        }
        Ok(())
    }

    /// Queues a job and hands it to its runner, or returns the active job it duplicates.
    pub async fn submit(self: &Arc<Self>, spec: JobSpec) -> Result<Job, JobError> {
        let runner = self.runner(spec.kind)?;
        let (job, created) = self.enqueue(spec).await?;
        if created {
            runner.run(self, vec![job.id], false);
        }
        Ok(job)
    }

    /// Hands an already queued job to its runner.
    pub fn dispatch(self: &Arc<Self>, job: &Job) -> Result<(), JobError> {
        self.runner(job.kind)?.run(self, vec![job.id], false);
        Ok(())
    }

    /// Runs a finished job again in place, resuming from what it already completed. When
    /// another active job has taken its dedupe key, that job is returned instead.
    pub async fn retry(self: &Arc<Self>, id: Uuid) -> Result<Job, JobError> {
        let runner = self
            .get(id)
            .await?
            .ok_or(JobError::NotFound(id))
            .and_then(|job| self.runner(job.kind))?;
        let job = self.requeue(id).await?;
        if job.id == id {
            runner.run(self, vec![id], true);
        }
        Ok(job)
    }

    fn runner(&self, kind: JobKind) -> Result<Arc<dyn JobRunner>, JobError> {
        self.runners
            .get(&kind)
            .map(|runner| runner.clone())
            .ok_or(JobError::Unsupported(kind))
    }
}

impl JobHandle {
    /// Finishes the job with how its work ended: an outcome records its final progress
    /// and state, and an error fails the job.
    pub async fn settle<E: Display>(&self, outcome: Result<JobOutcome, E>) -> Result<Job, JobError> {
        match outcome {
            Ok(JobOutcome { state, progress }) => {
                if let Some(progress) = progress {
                    self.progress(progress).await?;
                }
                self.finish(state, None).await
            }
            Err(error) => self.finish(JobState::Failed, Some(error.to_string())).await,
        }
    }
}
