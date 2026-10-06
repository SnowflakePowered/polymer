use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use serde_json::Value;
use std::{cell::RefCell, rc::Rc, sync::Arc};

use super::{PolymerHost, ServiceCall, ServiceError};

mod messages;
pub use messages::*;

#[op2]
#[serde]
pub(super) async fn op_library_import_key(
    state: Rc<RefCell<OpState>>,
    #[string] platform: String,
    #[string] namespace: String,
    #[serde] key: Vec<u8>,
    #[string] title_id: Option<String>,
) -> Result<ImportedKey, JsErrorBox> {
    Library::from_state(&state)?
        .import_key
        .call(LibraryImportKey {
            platform,
            namespace,
            key,
            title_id,
        })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_library_keys_list(
    state: Rc<RefCell<OpState>>,
    #[string] platform: String,
    #[string] namespace: Option<String>,
) -> Result<serde_json::Value, JsErrorBox> {
    Library::from_state(&state)?
        .keys_list
        .call(KeysList {
            platform,
            namespace,
        })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_library_keys_platforms(
    state: Rc<RefCell<OpState>>,
) -> Result<Vec<String>, JsErrorBox> {
    Library::from_state(&state)?
        .keys_platforms
        .call(KeysPlatforms)
        .await
}

#[op2]
pub(super) async fn op_library_keys_licensed(
    state: Rc<RefCell<OpState>>,
    #[string] platform: String,
    #[string] title_id: String,
) -> Result<bool, JsErrorBox> {
    Library::from_state(&state)?
        .keys_licensed
        .call(KeysLicensed { platform, title_id })
        .await
}

#[op2]
pub(super) async fn op_library_keys_remove(
    state: Rc<RefCell<OpState>>,
    #[string] platform: String,
    #[string] namespace: String,
    #[string] id: String,
) -> Result<bool, JsErrorBox> {
    Library::from_state(&state)?
        .keys_remove
        .call(KeysRemove {
            platform,
            namespace,
            id,
        })
        .await
}

impl ServiceError for LibraryError {
    fn unavailable(_: actix::MailboxError) -> JsErrorBox {
        JsErrorBox::generic("library service is unavailable")
    }

    fn into_js(self) -> JsErrorBox {
        match self {
            Self::Type(message) => JsErrorBox::type_error(message),
            Self::Generic(message) => JsErrorBox::generic(message),
        }
    }
}

impl Library {
    fn from_state(state: &Rc<RefCell<OpState>>) -> Result<Self, JsErrorBox> {
        let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
        host.library().map_err(JsErrorBox::generic)
    }
}

#[op2]
#[serde]
pub(super) async fn op_library_games_list(
    state: Rc<RefCell<OpState>>,
    #[serde] query: polymer_library::GameQuery,
) -> Result<serde_json::Value, JsErrorBox> {
    Library::from_state(&state)?
        .games_list
        .call(GamesList { query })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_library_games_platforms(
    state: Rc<RefCell<OpState>>,
) -> Result<Vec<String>, JsErrorBox> {
    Library::from_state(&state)?
        .games_platforms
        .call(GamesPlatforms)
        .await
}

#[op2]
#[serde]
pub(super) async fn op_library_games_get(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<serde_json::Value, JsErrorBox> {
    Library::from_state(&state)?
        .games_get
        .call(GamesGet { id })
        .await
}

#[op2]
pub(super) async fn op_library_games_delete(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<(), JsErrorBox> {
    Library::from_state(&state)?
        .games_delete
        .call(GamesDelete { id })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_library_launchables_add(
    state: Rc<RefCell<OpState>>,
    #[string] path: String,
    #[string] platform: Option<String>,
    #[string] parent_binary_id: Option<String>,
    #[serde] artifact: Option<InstalledArtifact>,
) -> Result<serde_json::Value, JsErrorBox> {
    Library::from_state(&state)?
        .launchables_add
        .call(LaunchablesAdd {
            path,
            platform,
            parent_binary_id,
            artifact,
        })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_library_launchables_find(
    state: Rc<RefCell<OpState>>,
    #[string] uri: String,
) -> Result<serde_json::Value, JsErrorBox> {
    Library::from_state(&state)?
        .launchables_find
        .call(LaunchablesFind { uri })
        .await
}

#[op2]
pub(super) async fn op_library_launchables_delete(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<(), JsErrorBox> {
    Library::from_state(&state)?
        .launchables_delete
        .call(LaunchablesDelete { id })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_library_metadata_add(
    state: Rc<RefCell<OpState>>,
    #[string] parent_binary_id: String,
    #[serde] input: serde_json::Value,
) -> Result<serde_json::Value, JsErrorBox> {
    Library::from_state(&state)?
        .metadata_add
        .call(MetadataAdd {
            parent_binary_id,
            input,
        })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_library_metadata_update(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
    #[serde] input: serde_json::Value,
) -> Result<serde_json::Value, JsErrorBox> {
    Library::from_state(&state)?
        .metadata_update
        .call(MetadataUpdate { id, input })
        .await
}

#[op2]
pub(super) async fn op_library_metadata_delete(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<(), JsErrorBox> {
    Library::from_state(&state)?
        .metadata_delete
        .call(MetadataDelete { id })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_library_metadata_replace(
    state: Rc<RefCell<OpState>>,
    #[string] parent_binary_id: String,
    #[string] source: String,
    #[serde] inputs: Vec<Value>,
) -> Result<serde_json::Value, JsErrorBox> {
    Library::from_state(&state)?
        .metadata_replace
        .call(MetadataReplace {
            parent_binary_id,
            source,
            inputs,
        })
        .await
}

#[op2]
#[string]
pub(super) async fn op_library_metadata_text(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<String, JsErrorBox> {
    Library::from_state(&state)?
        .metadata_text
        .call(MetadataText { id })
        .await
}

#[op2]
#[string]
pub(super) async fn op_library_metadata_url(
    state: Rc<RefCell<OpState>>,
    #[string] id: String,
) -> Result<String, JsErrorBox> {
    Library::from_state(&state)?
        .metadata_url
        .call(MetadataUrl { id })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_scraping_start(
    state: Rc<RefCell<OpState>>,
    #[string] root: String,
    #[string] trigger: String,
) -> Result<serde_json::Value, JsErrorBox> {
    Library::from_state(&state)?
        .scraping_start
        .call(ScrapingStart {
            root_binary_id: root,
            trigger_binary_id: trigger,
        })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_scraping_scrapers(
    state: Rc<RefCell<OpState>>,
) -> Result<serde_json::Value, JsErrorBox> {
    Library::from_state(&state)?
        .scraping_scrapers
        .call(ScrapingScrapers)
        .await
}

#[op2]
#[serde]
pub(super) async fn op_scraping_refresh(
    state: Rc<RefCell<OpState>>,
    #[string] plugin: String,
) -> Result<serde_json::Value, JsErrorBox> {
    Library::from_state(&state)?
        .scraping_refresh
        .call(ScrapingRefresh { plugin })
        .await
}
