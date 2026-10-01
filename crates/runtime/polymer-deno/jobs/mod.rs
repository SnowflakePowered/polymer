mod messages;
pub use messages::*;

use super::{PolymerHost, ServiceCall, ServiceError};
use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use std::{cell::RefCell, rc::Rc, sync::Arc};

impl ServiceError for JobsError {
    fn unavailable(error: actix::MailboxError) -> JsErrorBox {
        JsErrorBox::generic(format!("jobs actor unavailable: {error}"))
    }
}

impl Jobs {
    fn from_state(state: &Rc<RefCell<OpState>>) -> Result<Self, JsErrorBox> {
        let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
        host.jobs().map_err(JsErrorBox::generic)
    }
}

#[op2]
#[serde]
pub(super) async fn op_jobs_list(state: Rc<RefCell<OpState>>) -> Result<Vec<Job>, JsErrorBox> {
    Jobs::from_state(&state)?.list.call(JobsList).await
}

#[op2]
#[serde]
pub(super) async fn op_jobs_get(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<Option<Job>, JsErrorBox> {
    Jobs::from_state(&state)?.get.call(JobsGet { id }).await
}

#[op2]
#[serde]
pub(super) async fn op_jobs_cancel(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<Job, JsErrorBox> {
    Jobs::from_state(&state)?
        .cancel
        .call(JobsCancel { id })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_jobs_retry(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<Job, JsErrorBox> {
    Jobs::from_state(&state)?.retry.call(JobsRetry { id }).await
}

#[op2]
pub(super) async fn op_jobs_dismiss(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<(), JsErrorBox> {
    Jobs::from_state(&state)?
        .dismiss
        .call(JobsDismiss { id })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_jobs_import(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
    #[string] storage: Option<String>,
) -> Result<Job, JsErrorBox> {
    Jobs::from_state(&state)?
        .import
        .call(JobsImport { path, storage })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_jobs_check_updates(
    state: Rc<RefCell<OpState>>,
    #[string] launchable: String,
) -> Result<Job, JsErrorBox> {
    Jobs::from_state(&state)?
        .check_updates
        .call(JobsCheckUpdates { launchable })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_jobs_fetch_update(
    state: Rc<RefCell<OpState>>,
    #[string] launchable: String,
    #[serde] candidate: serde_json::Value,
) -> Result<Job, JsErrorBox> {
    Jobs::from_state(&state)?
        .fetch_update
        .call(JobsFetchUpdate {
            launchable,
            candidate,
        })
        .await
}
