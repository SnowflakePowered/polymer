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
) -> Result<StorageInspection, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .recover
        .send(Recover { platform, id })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_platforms(
    state: Rc<RefCell<OpState>>,
) -> Result<Vec<StoragePlatform>, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .platforms
        .send(Platforms)
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_inspect(
    state: Rc<RefCell<OpState>>,
    #[string] platform: String,
    #[string] id: String,
) -> Result<StorageInspection, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .inspect
        .send(Inspect { platform, id })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_install(
    state: Rc<RefCell<OpState>>,
    #[string] platform: String,
    #[string] id: String,
    #[string] path: String,
) -> Result<StorageArtifact, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .install
        .send(Install {
            platform,
            id,
            path: path.into(),
        })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_bootstrap(
    state: Rc<RefCell<OpState>>,
    #[string] platform: String,
    #[string] id: String,
    #[serde] settings: serde_json::Value,
) -> Result<StorageInspection, JsErrorBox> {
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
) -> Result<Vec<StorageArtifact>, JsErrorBox> {
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
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_plan_removal(
    state: Rc<RefCell<OpState>>,
    #[string] platform: String,
    #[string] id: String,
    #[serde] ids: Vec<String>,
) -> Result<StorageRemovalPlan, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .plan_removal
        .send(PlanRemoval { platform, id, ids })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_list(
    state: Rc<RefCell<OpState>>,
) -> Result<Vec<StorageVolume>, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .list
        .send(List)
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
pub(super) async fn op_consolestorage_supports(
    state: Rc<RefCell<OpState>>,
    #[string] kind: String,
    #[string] platform: String,
) -> Result<bool, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .supports
        .send(Supports { kind, platform })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_create(
    state: Rc<RefCell<OpState>>,
    #[string] kind: String,
    #[string] platform: String,
    #[string] label: Option<String>,
) -> Result<StorageVolume, JsErrorBox> {
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
        .map_err(JsErrorBox::generic)
}

#[op2]
pub(super) async fn op_consolestorage_delete(
    state: Rc<RefCell<OpState>>,
    #[string] kind: String,
    #[string] platform: String,
    #[string] id: String,
) -> Result<(), JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .delete
        .send(Delete { kind, platform, id })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
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
) -> Result<StorageVolume, JsErrorBox> {
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
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_fork(
    state: Rc<RefCell<OpState>>,
    #[serde] volume: VolumeRef,
    #[string] label: Option<String>,
) -> Result<StorageVolume, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .fork
        .send(Fork { volume, label })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_saves(
    state: Rc<RefCell<OpState>>,
    #[serde] volume: VolumeRef,
) -> Result<Vec<StorageSave>, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .saves
        .send(ListSaves { volume })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_save_history(
    state: Rc<RefCell<OpState>>,
    #[serde] volume: VolumeRef,
    #[string] save: String,
) -> Result<Vec<StorageSaveCommit>, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .save_history
        .send(SaveHistory { volume, save })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_save_restore(
    state: Rc<RefCell<OpState>>,
    #[serde] volume: VolumeRef,
    #[string] save: String,
    #[string] commit: String,
) -> Result<StorageSaveCommit, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .restore_save
        .send(RestoreSave { volume, save, commit })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_save_copy(
    state: Rc<RefCell<OpState>>,
    #[serde] volume: VolumeRef,
    #[string] save: String,
    #[serde] destination: VolumeRef,
) -> Result<StorageSaveCommit, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .copy_save
        .send(CopySave { volume, save, destination })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}

#[op2]
pub(super) async fn op_consolestorage_save_delete(
    state: Rc<RefCell<OpState>>,
    #[serde] volume: VolumeRef,
    #[string] save: String,
) -> Result<(), JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .delete_save
        .send(DeleteSave { volume, save })
        .await
        .map_err(|error| format!("storage actor unavailable: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()))
        .map_err(JsErrorBox::generic)
}
