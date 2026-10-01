use deno_core::{OpState, Resource, ResourceId, op2};
use deno_error::JsErrorBox;
use polymer_library::jobs::{
    JobHandle, JobKind, JobOutcome, JobProgress, JobSpec, JobStore, JobSubject,
};
use std::{cell::RefCell, rc::Rc, sync::Arc};
use uuid::Uuid;

/// The job store a plugin reports to.
#[derive(Clone)]
pub(crate) struct PluginJobs {
    pub store: Option<Arc<JobStore>>,
    pub plugin: String,
}

impl PluginJobs {
    /// Registers `job` for the isolate and returns its handle for the plugin's `Job` scope.
    /// The host's job is the root scope of an invocation; plugin jobs are added by `Job`.
    pub(crate) fn register(state: &mut OpState, job: Arc<JobHandle>) -> ResourceId {
        state.resource_table.add(PluginJob(job))
    }

    /// The job registered as `rid`, which jobs started under that scope name as their parent.
    pub(crate) fn parent(state: &OpState, rid: Option<ResourceId>) -> Option<Uuid> {
        let rid = rid?;
        Some(state.resource_table.get::<PluginJob>(rid).ok()?.0.id())
    }

    /// Starts an in-memory job of `kind` for this plugin under `parent`.
    pub(crate) async fn start(
        &self,
        kind: JobKind,
        title: String,
        subject: JobSubject,
        parent: Option<Uuid>,
    ) -> Option<Arc<JobHandle>> {
        let spec = JobSpec {
            kind,
            parent,
            title,
            subject: JobSubject {
                plugin_id: Some(self.plugin.clone()),
                ..subject
            },
            params: serde_json::Value::Null,
            dedupe: None,
            steps: Vec::new(),
            persistent: false,
        };
        let job = self.store.as_ref()?.start(spec).await.map_err(|error| {
            tracing::warn!(%error, plugin = %self.plugin, "could not start plugin job");
        });
        Some(Arc::new(job.ok()?))
    }
}

/// A job registered with the isolate. The isolate dropping a plugin's job unfinished fails it.
struct PluginJob(Arc<JobHandle>);

impl Resource for PluginJob {}

/// Starts an in-memory job that the plugin reports on and finishes itself.
#[op2]
#[serde]
pub(super) async fn op_job_start(
    state: Rc<RefCell<OpState>>,
    #[string] title: String,
    #[smi] parent: Option<ResourceId>,
) -> Option<ResourceId> {
    let (jobs, parent) = {
        let state = state.borrow();
        let jobs = state.try_borrow::<PluginJobs>().cloned()?;
        (jobs, PluginJobs::parent(&state, parent))
    };
    let job = jobs
        .start(JobKind::Plugin, title, JobSubject::default(), parent)
        .await?;
    Some(PluginJobs::register(&mut state.borrow_mut(), job))
}

#[op2]
pub(super) async fn op_job_report(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: ResourceId,
    #[string] message: Option<String>,
    completed: Option<f64>,
    total: Option<f64>,
) {
    let Ok(job) = state.borrow().resource_table.get::<PluginJob>(rid) else {
        return;
    };
    let progress = JobProgress {
        message,
        completed: completed.map(|value| value as u64),
        total: total.map(|value| value as u64),
        ..Default::default()
    };
    if let Err(error) = job.0.progress(progress).await {
        tracing::warn!(%error, job = %job.0.id(), "could not record plugin progress");
    }
}

#[op2]
pub(super) async fn op_job_finish(
    state: Rc<RefCell<OpState>>,
    #[smi] rid: ResourceId,
    #[string] error: Option<String>,
) -> Result<(), JsErrorBox> {
    let job = state
        .borrow_mut()
        .resource_table
        .take::<PluginJob>(rid)
        .map_err(|_| JsErrorBox::type_error("job has already finished"))?;
    job.0
        .settle(error.map_or(Ok(JobOutcome::completed(None)), Err))
        .await
        .map(drop)
        .map_err(|error| JsErrorBox::generic(error.to_string()))
}
