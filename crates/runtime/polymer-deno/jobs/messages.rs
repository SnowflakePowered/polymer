use actix::{Message, Recipient};
pub use polymer_library::jobs::Job;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct JobsError(pub String);

impl JobsError {
    pub fn from_display(error: impl std::fmt::Display) -> Self {
        Self(error.to_string())
    }
}

/// Typed job endpoints exposed to Deno without depending on the backend.
#[derive(Clone, Debug)]
pub struct Jobs {
    pub list: Recipient<JobsList>,
    pub get: Recipient<JobsGet>,
    pub cancel: Recipient<JobsCancel>,
    pub retry: Recipient<JobsRetry>,
    pub dismiss: Recipient<JobsDismiss>,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Vec<Job>, JobsError>")]
pub struct JobsList;

#[derive(Debug, Message)]
#[rtype(result = "Result<Option<Job>, JobsError>")]
pub struct JobsGet {
    pub id: String,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Job, JobsError>")]
pub struct JobsCancel {
    pub id: String,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Job, JobsError>")]
pub struct JobsRetry {
    pub id: String,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<(), JobsError>")]
pub struct JobsDismiss {
    pub id: String,
}
