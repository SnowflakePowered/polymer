use super::PluginInvocationError;
use deno_core::{OpState, op2};
use polymer_library::jobs::{
    JobHandle, JobKind, JobProgress, JobSpec, JobState, JobStore, JobSubject,
};
use std::{cell::RefCell, rc::Rc, sync::Arc};

/// The job store a plugin reports to, and the job its current invocation reports on.
#[derive(Clone)]
pub(crate) struct PluginJobs {
    pub store: Option<Arc<JobStore>>,
    pub plugin: String,
    pub current: Option<Arc<JobHandle>>,
}

impl PluginJobs {
    /// Starts an in-memory job for work that cannot outlive the process.
    pub(crate) async fn start(&self, message: &str) -> Option<Arc<JobHandle>> {
        let spec = JobSpec {
            kind: JobKind::Plugin,
            title: self.plugin.clone(),
            subject: JobSubject {
                plugin_id: Some(self.plugin.clone()),
                ..Default::default()
            },
            params: serde_json::Value::Null,
            dedupe: None,
            steps: Vec::new(),
            persistent: false,
        };
        let progress = JobProgress {
            message: Some(message.to_owned()),
            ..Default::default()
        };
        let started = async {
            let job = self.store.as_ref()?.start(spec).await.map_err(|error| {
                tracing::warn!(%error, plugin = %self.plugin, "could not start plugin job");
            });
            let job = Arc::new(job.ok()?);
            let _ = job.progress(progress).await;
            Some(job)
        };
        started.await
    }

    pub(crate) async fn finish<T>(
        job: Option<Arc<JobHandle>>,
        result: &Result<T, PluginInvocationError>,
    ) {
        let Some(job) = job else {
            return;
        };
        let (state, error) = match result {
            Ok(_) => (JobState::Completed, None),
            Err(error) => (JobState::Failed, Some(error.to_string())),
        };
        if let Err(error) = job.finish(state, error).await {
            tracing::warn!(%error, job = %job.id(), "could not finish plugin job");
        }
    }

    /// Makes `job` the one plugin progress reports on, returning the previous one.
    pub(crate) fn swap(state: &mut OpState, job: Option<Arc<JobHandle>>) -> Option<Arc<JobHandle>> {
        state
            .try_borrow_mut::<Self>()
            .and_then(|jobs| std::mem::replace(&mut jobs.current, job))
    }

    pub(crate) fn current(state: &OpState) -> Option<Arc<JobHandle>> {
        state
            .try_borrow::<Self>()
            .and_then(|jobs| jobs.current.clone())
    }
}

#[op2]
pub(super) async fn op_job_progress(
    state: Rc<RefCell<OpState>>,
    #[string] message: String,
    completed: Option<f64>,
    total: Option<f64>,
) {
    let Some(job) = PluginJobs::current(&state.borrow()) else {
        return;
    };
    let progress = JobProgress {
        message: Some(message),
        completed: completed.map(|value| value as u64),
        total: total.map(|value| value as u64),
        ..Default::default()
    };
    if let Err(error) = job.progress(progress).await {
        tracing::warn!(%error, job = %job.id(), "could not record plugin progress");
    }
}
