use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::JobError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    Queued,
    Running,
    Paused,
    Completed,
    Partial,
    Failed,
    Cancelled,
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    Scrape,
    Install,
    Bootstrap,
    Plugin,
    Import,
    Check,
    Update,
    Download,
    Refresh,
    Scan,
    Extract,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSubject {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobStep {
    pub key: String,
    pub state: JobState,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    #[serde(serialize_with = "Job::serialize_id")]
    pub id: Uuid,
    pub kind: JobKind,
    pub state: JobState,
    /// The job this one is part of. Progress stays on each job; frontends nest children.
    #[serde(serialize_with = "Job::serialize_parent")]
    pub parent: Option<Uuid>,
    pub title: String,
    pub icon: Option<String>,
    pub subject: JobSubject,
    pub params: Value,
    pub message: Option<String>,
    pub completed: Option<u64>,
    pub total: Option<u64>,
    pub detail: Value,
    pub error: Option<String>,
    pub persistent: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub steps: Vec<JobStep>,
    #[serde(skip)]
    pub dedupe: Option<String>,
}

/// A job to create. Persistent jobs with a `dedupe` key are unique while active.
#[derive(Clone, Debug)]
pub struct JobSpec {
    pub kind: JobKind,
    pub parent: Option<Uuid>,
    pub title: String,
    pub subject: JobSubject,
    pub params: Value,
    pub dedupe: Option<String>,
    pub steps: Vec<String>,
    pub persistent: bool,
}

#[derive(Clone, Debug, Default)]
pub struct JobProgress {
    pub title: Option<String>,
    pub icon: Option<String>,
    pub message: Option<String>,
    pub completed: Option<u64>,
    pub total: Option<u64>,
    pub detail: Option<Value>,
}

impl JobState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub const fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Paused)
    }

    pub(super) fn parse(value: &str) -> Result<Self, JobError> {
        serde_json::from_value(Value::from(value))
            .map_err(|_| JobError::InvalidData(format!("unknown job state '{value}'")))
    }
}

impl JobKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scrape => "scrape",
            Self::Install => "install",
            Self::Bootstrap => "bootstrap",
            Self::Plugin => "plugin",
            Self::Import => "import",
            Self::Check => "check",
            Self::Update => "update",
            Self::Download => "download",
            Self::Refresh => "refresh",
            Self::Scan => "scan",
            Self::Extract => "extract",
        }
    }

    pub(super) fn parse(value: &str) -> Result<Self, JobError> {
        serde_json::from_value(Value::from(value))
            .map_err(|_| JobError::InvalidData(format!("unknown job kind '{value}'")))
    }
}

impl Job {
    fn serialize_id<S: serde::Serializer>(value: &Uuid, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.simple().to_string())
    }

    fn serialize_parent<S: serde::Serializer>(
        value: &Option<Uuid>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(value) => Self::serialize_id(value, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub(super) fn new(id: Uuid, spec: &JobSpec, now: i64) -> Self {
        Self {
            id,
            kind: spec.kind,
            state: JobState::Queued,
            parent: spec.parent,
            title: spec.title.clone(),
            icon: None,
            subject: spec.subject.clone(),
            params: spec.params.clone(),
            message: None,
            completed: None,
            total: None,
            detail: Value::Null,
            error: None,
            persistent: spec.persistent,
            created_at: now,
            updated_at: now,
            steps: spec
                .steps
                .iter()
                .map(|key| JobStep {
                    key: key.clone(),
                    state: JobState::Queued,
                    error: None,
                })
                .collect(),
            dedupe: spec.dedupe.clone(),
        }
    }

    pub(super) fn apply(&mut self, progress: JobProgress, now: i64) {
        self.title = progress.title.unwrap_or(std::mem::take(&mut self.title));
        self.icon = progress.icon.or(self.icon.take());
        self.message = progress.message.or(self.message.take());
        self.completed = progress.completed.or(self.completed);
        self.total = progress.total.or(self.total);
        self.detail = progress.detail.unwrap_or(std::mem::take(&mut self.detail));
        self.updated_at = now;
    }

    pub(super) fn step(&mut self, key: &str, state: JobState, error: Option<&str>, now: i64) {
        let error = error.map(str::to_owned);
        match self.steps.iter_mut().find(|step| step.key == key) {
            Some(step) => {
                step.state = state;
                step.error = error;
            }
            None => self.steps.push(JobStep {
                key: key.to_owned(),
                state,
                error,
            }),
        }
        self.updated_at = now;
    }
}
