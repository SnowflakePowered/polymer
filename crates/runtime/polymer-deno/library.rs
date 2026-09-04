use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use serde_json::Value;
use std::{cell::RefCell, rc::Rc, sync::Arc};

use super::PolymerHost;

polymer_command::command_client! {
    pub client Library {
        closed: "library service is unavailable".to_owned();

        #[args(GamesList)]
        pub async fn games_list(platform: Option<String>) -> Result<Value, String>;

        #[args(GamesGet)]
        pub async fn games_get(id: String) -> Result<Value, String>;

        #[args(GamesDelete)]
        pub async fn games_delete(id: String) -> Result<(), String>;

        #[args(LaunchablesAdd)]
        pub async fn launchables_add(
            path: String,
            platform: Option<String>,
            parent_binary_id: Option<String>,
        ) -> Result<Value, String>;

        #[args(LaunchablesDelete)]
        pub async fn launchables_delete(id: String) -> Result<(), String>;

        #[args(MetadataAdd)]
        pub async fn metadata_add(
            parent_binary_id: String,
            input: Value,
        ) -> Result<Value, String>;

        #[args(MetadataUpdate)]
        pub async fn metadata_update(id: String, input: Value) -> Result<Value, String>;

        #[args(MetadataDelete)]
        pub async fn metadata_delete(id: String) -> Result<(), String>;

        #[args(MetadataReplace)]
        pub async fn metadata_replace(
            parent_binary_id: String,
            source: String,
            inputs: Vec<Value>,
        ) -> Result<Value, String>;

        #[args(MetadataText)]
        pub async fn metadata_text(id: String) -> Result<String, String>;

        #[args(MetadataUrl)]
        pub async fn metadata_url(id: String) -> Result<String, String>;

        #[args(ScrapingJobs)]
        pub async fn scraping_jobs() -> Result<Value, String>;
        #[args(ScrapingStart)]
        pub async fn scraping_start(root_binary_id: String, trigger_binary_id: String) -> Result<Value, String>;
        #[args(ScrapingJob)]
        pub async fn scraping_job(id: String) -> Result<Value, String>;
        #[args(ScrapingRetry)]
        pub async fn scraping_retry(id: String) -> Result<Value, String>;
        #[args(ScrapingScrapers)]
        pub async fn scraping_scrapers() -> Result<Value, String>;
        #[args(ScrapingRefresh)]
        pub async fn scraping_refresh(plugin: String) -> Result<(), String>;
    }
}

impl Library {
    pub fn channel() -> (Self, tokio::sync::mpsc::UnboundedReceiver<LibraryCommand>) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        (Self::new(sender), receiver)
    }
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
    library(&state)?.games_list(platform).await.map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_library_games_get(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?.games_get(id).await.map_err(error)
}

#[op2]
pub(super) async fn op_library_games_delete(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<(), JsErrorBox> {
    library(&state)?.games_delete(id).await.map_err(error)
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
        .launchables_add(path, platform, parent_binary_id)
        .await
        .map_err(error)
}

#[op2]
pub(super) async fn op_library_launchables_delete(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<(), JsErrorBox> {
    library(&state)?.launchables_delete(id).await.map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_library_metadata_add(
    state: Rc<RefCell<OpState>>,
    #[string] parent_binary_id: String,
    #[serde] input: serde_json::Value,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?
        .metadata_add(parent_binary_id, input)
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
        .metadata_update(id, input)
        .await
        .map_err(error)
}

#[op2]
pub(super) async fn op_library_metadata_delete(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<(), JsErrorBox> {
    library(&state)?.metadata_delete(id).await.map_err(error)
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
        .metadata_replace(parent_binary_id, source, inputs)
        .await
        .map_err(error)
}

#[op2]
#[string]
pub(super) async fn op_library_metadata_text(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<String, JsErrorBox> {
    library(&state)?.metadata_text(id).await.map_err(error)
}

#[op2]
#[string]
pub(super) async fn op_library_metadata_url(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<String, JsErrorBox> {
    library(&state)?.metadata_url(id).await.map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_scraping_jobs(
    state: Rc<RefCell<OpState>>,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?.scraping_jobs().await.map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_scraping_start(
    state: Rc<RefCell<OpState>>,
    #[string] root: String,
    #[string] trigger: String,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?
        .scraping_start(root, trigger)
        .await
        .map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_scraping_job(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?.scraping_job(id).await.map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_scraping_retry(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?.scraping_retry(id).await.map_err(error)
}

#[op2]
#[serde]
pub(super) async fn op_scraping_scrapers(
    state: Rc<RefCell<OpState>>,
) -> Result<serde_json::Value, JsErrorBox> {
    library(&state)?.scraping_scrapers().await.map_err(error)
}

#[op2]
pub(super) async fn op_scraping_refresh(
    state: Rc<RefCell<OpState>>,
    #[string] plugin: String,
) -> Result<(), JsErrorBox> {
    library(&state)?
        .scraping_refresh(plugin)
        .await
        .map_err(error)
}
