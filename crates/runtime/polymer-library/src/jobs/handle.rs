use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

use super::{Job, JobError, JobProgress, JobState, JobStore};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// Reports on a claimed job. Dropping an unfinished handle causes the job to fail.
pub struct JobHandle {
    store: Arc<JobStore>,
    id: Uuid,
    finished: AtomicBool,
    pending: Mutex<(Option<Instant>, Option<JobProgress>)>,
}

impl JobHandle {
    pub(super) fn new(store: Arc<JobStore>, id: Uuid) -> Self {
        Self {
            store,
            id,
            finished: AtomicBool::new(false),
            pending: Mutex::new((None, None)),
        }
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn store(&self) -> &Arc<JobStore> {
        &self.store
    }

    pub async fn job(&self) -> Result<Job, JobError> {
        self.store
            .get(self.id)
            .await?
            .ok_or(JobError::NotFound(self.id))
    }

    /// Records progress, coalescing updates that arrive faster than the store is written.
    pub async fn progress(&self, progress: JobProgress) -> Result<(), JobError> {
        let ready = {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let merged = match pending.1.take() {
                Some(previous) => previous.merge(progress),
                None => progress,
            };
            let complete = merged.total.is_some() && merged.completed == merged.total;
            if complete
                || pending
                    .0
                    .is_none_or(|last| last.elapsed() >= PROGRESS_INTERVAL)
            {
                pending.0 = Some(Instant::now());
                Some(merged)
            } else {
                pending.1 = Some(merged);
                None
            }
        };
        if let Some(progress) = ready {
            self.store
                .modify(self.id, |job| job.apply(progress, JobStore::now()))
                .await?;
        }
        Ok(())
    }

    /// A synchronous progress sink that forwards updates to this job in order.
    pub fn reporter(self: &Arc<Self>) -> impl Fn(JobProgress) + Send + Sync + 'static + use<> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<JobProgress>();
        let job = self.clone();
        tokio::spawn(async move {
            while let Some(progress) = receiver.recv().await {
                if let Err(error) = job.progress(progress).await {
                    tracing::warn!(%error, job = %job.id, "could not record job progress");
                }
            }
        });
        move |progress| {
            let _ = sender.send(progress);
        }
    }

    /// Writes the job's detail immediately, so a resumed job can rely on it.
    pub async fn record(&self, detail: serde_json::Value) -> Result<Job, JobError> {
        self.store.modify(self.id, |job| job.detail = detail).await
    }

    /// Replaces the job's parameters, recording a choice made while it runs.
    pub async fn reparameterize(&self, params: serde_json::Value) -> Result<Job, JobError> {
        self.store.modify(self.id, |job| job.params = params).await
    }

    pub async fn step(
        &self,
        key: &str,
        state: JobState,
        error: Option<&str>,
    ) -> Result<Job, JobError> {
        self.store
            .modify(self.id, |job| job.step(key, state, error, JobStore::now()))
            .await
    }

    pub async fn finish(&self, state: JobState, error: Option<String>) -> Result<Job, JobError> {
        self.finished.store(true, Ordering::Release);
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .1
            .take();
        self.store
            .modify(self.id, |job| {
                if let Some(progress) = pending {
                    job.apply(progress, JobStore::now());
                }
                job.state = state;
                job.error = error;
            })
            .await
    }
}

impl Drop for JobHandle {
    fn drop(&mut self) {
        if self.finished.load(Ordering::Acquire) || self.store.interrupt_ephemeral(self.id) {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (store, id) = (self.store.clone(), self.id);
        runtime.spawn(async move {
            let result = store
                .modify(id, |job| {
                    job.state = JobState::Failed;
                    job.error = Some("Interrupted".to_owned());
                })
                .await;
            if let Err(error) = result {
                tracing::warn!(%error, job = %id, "could not record interrupted job");
            }
        });
    }
}

impl JobProgress {
    fn merge(self, newer: Self) -> Self {
        Self {
            title: newer.title.or(self.title),
            icon: newer.icon.or(self.icon),
            message: newer.message.or(self.message),
            completed: newer.completed.or(self.completed),
            total: newer.total.or(self.total),
            detail: newer.detail.or(self.detail),
        }
    }
}
