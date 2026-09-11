use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use serde_json::Value;
use std::{cell::RefCell, rc::Rc, sync::Arc};

use super::PolymerHost;

mod messages;
pub use messages::*;

fn library(state: &Rc<RefCell<OpState>>) -> Result<Library, JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.library()
        .ok_or_else(|| JsErrorBox::generic("library service is unavailable"))
}

fn error<M>(error: kameo::error::SendError<M, String>) -> JsErrorBox {
    let error = match error {
        kameo::error::SendError::HandlerError(error) => error,
        _ => return JsErrorBox::generic("library service is unavailable"),
    };
    if let Some(message) = error.strip_prefix("TypeError: ") {
        JsErrorBox::type_error(message.to_owned())
    } else {
        JsErrorBox::generic(error)
    }
}

#[op2]
#[serde]
pub(super) async fn op_library_games_list(
    state: Rc<RefCell<OpState>>,
    #[string] platform: Option<String>,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?
        .games_list
        .ask(GamesList { platform })
        .send()
        .await
        .map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_library_games_get(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?
        .games_get
        .ask(GamesGet { id })
        .send()
        .await
        .map_err(error)
}

#[op2]
pub(super) async fn op_library_games_delete(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<(), JsErrorBox> {
    library(&state)?
        .games_delete
        .ask(GamesDelete { id })
        .send()
        .await
        .map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_library_launchables_add(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
    #[string] platform: Option<String>,
    #[string] parent_binary_id: Option<String>,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?
        .launchables_add
        .ask(LaunchablesAdd {
            path,
            platform,
            parent_binary_id,
        })
        .send()
        .await
        .map_err(error)
}

#[op2]
pub(super) async fn op_library_launchables_delete(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<(), JsErrorBox> {
    library(&state)?
        .launchables_delete
        .ask(LaunchablesDelete { id })
        .send()
        .await
        .map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_library_metadata_add(
    state: Rc<RefCell<OpState>>,
    #[string] parent_binary_id: String,
    #[serde] input: serde_json::Value,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?
        .metadata_add
        .ask(MetadataAdd {
            parent_binary_id,
            input,
        })
        .send()
        .await
        .map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_library_metadata_update(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
    #[serde] input: serde_json::Value,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?
        .metadata_update
        .ask(MetadataUpdate { id, input })
        .send()
        .await
        .map_err(error)
}

#[op2]
pub(super) async fn op_library_metadata_delete(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<(), JsErrorBox> {
    library(&state)?
        .metadata_delete
        .ask(MetadataDelete { id })
        .send()
        .await
        .map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_library_metadata_replace(
    state: Rc<RefCell<OpState>>,
    #[string] parent_binary_id: String,
    #[string] source: String,
    #[serde] inputs: Vec<Value>,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?
        .metadata_replace
        .ask(MetadataReplace {
            parent_binary_id,
            source,
            inputs,
        })
        .send()
        .await
        .map_err(error)
}

#[op2]
#[string]
pub(super) async fn op_library_metadata_text(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<String, JsErrorBox> {
    library(&state)?
        .metadata_text
        .ask(MetadataText { id })
        .send()
        .await
        .map_err(error)
}

#[op2]
#[string]
pub(super) async fn op_library_metadata_url(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<String, JsErrorBox> {
    library(&state)?
        .metadata_url
        .ask(MetadataUrl { id })
        .send()
        .await
        .map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_scraping_jobs(
    state: Rc<RefCell<OpState>>,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?
        .scraping_jobs
        .ask(ScrapingJobs)
        .send()
        .await
        .map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_scraping_start(
    state: Rc<RefCell<OpState>>,
    #[string] root: String,
    #[string] trigger: String,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?
        .scraping_start
        .ask(ScrapingStart {
            root_binary_id: root,
            trigger_binary_id: trigger,
        })
        .send()
        .await
        .map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_scraping_job(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?
        .scraping_job
        .ask(ScrapingJob { id })
        .send()
        .await
        .map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_scraping_retry(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?
        .scraping_retry
        .ask(ScrapingRetry { id })
        .send()
        .await
        .map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_scraping_scrapers(
    state: Rc<RefCell<OpState>>,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?
        .scraping_scrapers
        .ask(ScrapingScrapers)
        .send()
        .await
        .map_err(error)
}

#[op2]
pub(super) async fn op_scraping_refresh(
    state: Rc<RefCell<OpState>>,
    #[string] plugin: String,
) -> Result<(), JsErrorBox> {
    library(&state)?
        .scraping_refresh
        .ask(ScrapingRefresh { plugin })
        .send()
        .await
        .map_err(error)
}
