use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use serde_json::Value;
use std::{cell::RefCell, rc::Rc, sync::Arc};

use super::PolymerHost;

mod messages;
pub use messages::*;

#[op2]
#[serde]
pub(super) async fn op_library_import(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?
        .import
        .send(LibraryImport { path })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
        .map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_library_import_key(
    state: Rc<RefCell<OpState>>,
    #[string] platform: String,
    #[string] namespace: String,
    #[serde] key: Vec<u8>,
    #[string] title_id: Option<String>,
) -> Result<ImportedKey, JsErrorBox> {
    library(&state)?
        .import_key
        .send(LibraryImportKey {
            platform,
            namespace,
            key,
            title_id,
        })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
        .map_err(error)
}

fn library(state: &Rc<RefCell<OpState>>) -> Result<Library, JsErrorBox> {
    let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
    host.library()
        .ok_or_else(|| JsErrorBox::generic("library service is unavailable"))
}

fn error(error: String) -> JsErrorBox {
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
        .send(GamesList { platform })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
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
        .send(GamesGet { id })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
        .map_err(error)
}

#[op2]
pub(super) async fn op_library_games_delete(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<(), JsErrorBox> {
    library(&state)?
        .games_delete
        .send(GamesDelete { id })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
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
        .send(LaunchablesAdd {
            path,
            platform,
            parent_binary_id,
        })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
        .map_err(error)
}

#[op2]
pub(super) async fn op_library_launchables_delete(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<(), JsErrorBox> {
    library(&state)?
        .launchables_delete
        .send(LaunchablesDelete { id })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
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
        .send(MetadataAdd {
            parent_binary_id,
            input,
        })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
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
        .send(MetadataUpdate { id, input })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
        .map_err(error)
}

#[op2]
pub(super) async fn op_library_metadata_delete(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<(), JsErrorBox> {
    library(&state)?
        .metadata_delete
        .send(MetadataDelete { id })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
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
        .send(MetadataReplace {
            parent_binary_id,
            source,
            inputs,
        })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
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
        .send(MetadataText { id })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
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
        .send(MetadataUrl { id })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
        .map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_scraping_jobs(
    state: Rc<RefCell<OpState>>,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?
        .scraping_jobs
        .send(ScrapingJobs)
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
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
        .send(ScrapingStart {
            root_binary_id: root,
            trigger_binary_id: trigger,
        })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
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
        .send(ScrapingJob { id })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
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
        .send(ScrapingRetry { id })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
        .map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_scraping_scrapers(
    state: Rc<RefCell<OpState>>,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?
        .scraping_scrapers
        .send(ScrapingScrapers)
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
        .map_err(error)
}

#[op2]
pub(super) async fn op_scraping_refresh(
    state: Rc<RefCell<OpState>>,
    #[string] plugin: String,
) -> Result<(), JsErrorBox> {
    library(&state)?
        .scraping_refresh
        .send(ScrapingRefresh { plugin })
        .await
        .map_err(|_| "library service is unavailable".to_owned())
        .and_then(|result| result)
        .map_err(error)
}
