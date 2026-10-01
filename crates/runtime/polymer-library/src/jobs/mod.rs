//! Durable, resumable background jobs shared by every job source.

mod handle;
mod model;
mod store;

pub use handle::JobHandle;
pub use model::{Job, JobKind, JobProgress, JobSpec, JobState, JobStep, JobSubject};
pub use store::JobStore;

use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error("the jobs database path is not valid UTF-8")]
    InvalidDatabasePath,
    #[error("jobs database error: {0}")]
    Database(#[from] turso::Error),
    #[error("jobs I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid job data: {0}")]
    InvalidData(String),
    #[error("job {} was not found", .0.simple())]
    NotFound(Uuid),
    #[error("job {} is {}", .0.simple(), .1.as_str())]
    InvalidState(Uuid, JobState),
}
