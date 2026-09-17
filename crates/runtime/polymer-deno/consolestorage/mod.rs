mod messages;
pub use messages::*;

use super::PolymerHost;
use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use std::{cell::RefCell, rc::Rc, sync::Arc};

impl ConsoleStorage {
    fn from_state(state: &Rc<RefCell<OpState>>) -> Result<Self, JsErrorBox> {
        let host = state.borrow().borrow::<Arc<dyn PolymerHost>>().clone();
        host.consolestorage().map_err(JsErrorBox::generic)
    }
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_recover(
    state: Rc<RefCell<OpState>>,
    #[string] platform: String,
    #[string] id: String,
) -> Result<serde_json::Value, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .recover
        .send(Recover { platform, id })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_platforms(
    state: Rc<RefCell<OpState>>,
) -> Result<serde_json::Value, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .platforms
        .send(Platforms)
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_inspect(
    state: Rc<RefCell<OpState>>,
    #[string] platform: String,
    #[string] id: String,
) -> Result<serde_json::Value, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .inspect
        .send(Inspect { platform, id })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_install(
    state: Rc<RefCell<OpState>>,
    #[string] platform: String,
    #[string] id: String,
    #[serde] source: serde_json::Value,
) -> Result<serde_json::Value, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .install
        .send(Install {
            platform,
            id,
            source,
        })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_bootstrap(
    state: Rc<RefCell<OpState>>,
    #[string] platform: String,
    #[string] id: String,
    #[serde] settings: serde_json::Value,
) -> Result<serde_json::Value, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .bootstrap
        .send(Bootstrap {
            platform,
            id,
            settings,
        })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_uninstall(
    state: Rc<RefCell<OpState>>,
    #[string] platform: String,
    #[string] id: String,
    #[serde] ids: Vec<String>,
    #[string] revision: String,
) -> Result<serde_json::Value, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .uninstall
        .send(Uninstall {
            platform,
            id,
            ids,
            revision,
        })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_plan_removal(
    state: Rc<RefCell<OpState>>,
    #[string] platform: String,
    #[string] id: String,
    #[serde] ids: Vec<String>,
) -> Result<serde_json::Value, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .plan_removal
        .send(PlanRemoval { platform, id, ids })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_list(
    state: Rc<RefCell<OpState>>,
) -> Result<serde_json::Value, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .list
        .send(List)
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_supports(
    state: Rc<RefCell<OpState>>,
    #[string] kind: String,
    #[string] platform: String,
) -> Result<serde_json::Value, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .supports
        .send(Supports { kind, platform })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_create(
    state: Rc<RefCell<OpState>>,
    #[string] kind: String,
    #[string] platform: String,
    #[string] label: Option<String>,
) -> Result<serde_json::Value, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .create
        .send(Create {
            kind,
            platform,
            label,
        })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_delete(
    state: Rc<RefCell<OpState>>,
    #[string] kind: String,
    #[string] platform: String,
    #[string] id: String,
) -> Result<serde_json::Value, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .delete
        .send(Delete { kind, platform, id })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_set_label(
    state: Rc<RefCell<OpState>>,
    #[string] kind: String,
    #[string] platform: String,
    #[string] id: String,
    #[string] label: Option<String>,
) -> Result<serde_json::Value, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .set_label
        .send(SetLabel {
            kind,
            platform,
            id,
            label,
        })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .and_then(|value| serde_json::to_value(value).map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}
