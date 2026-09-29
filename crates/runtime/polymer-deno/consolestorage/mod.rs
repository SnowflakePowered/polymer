mod messages;
pub use messages::*;

use super::{PolymerHost, ServiceCall, ServiceError};
use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use std::{cell::RefCell, rc::Rc, sync::Arc};

impl ServiceError for StorageError {
    fn unavailable(error: actix::MailboxError) -> JsErrorBox {
        JsErrorBox::generic(format!("storage actor unavailable: {error}"))
    }
}

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
        .call(Recover { platform, id })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_platforms(
    state: Rc<RefCell<OpState>>,
) -> Result<Vec<StoragePlatform>, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .platforms
        .call(Platforms)
        .await
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
        .call(Inspect { platform, id })
        .await
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
        .call(Install {
            platform,
            id,
            path: path.into(),
        })
        .await
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
        .call(Bootstrap {
            platform,
            id,
            settings,
        })
        .await
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
        .call(Uninstall {
            platform,
            id,
            ids,
            revision,
        })
        .await
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
        .call(PlanRemoval { platform, id, ids })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_list(
    state: Rc<RefCell<OpState>>,
) -> Result<Vec<StorageVolume>, JsErrorBox> {
    ConsoleStorage::from_state(&state)?.list.call(List).await
}

#[op2]
pub(super) async fn op_consolestorage_supports(
    state: Rc<RefCell<OpState>>,
    #[string] kind: String,
    #[string] platform: String,
) -> Result<bool, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .supports
        .call(Supports { kind, platform })
        .await
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
        .call(Create {
            kind,
            platform,
            label,
        })
        .await
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
        .call(Delete { kind, platform, id })
        .await
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
        .call(SetLabel {
            kind,
            platform,
            id,
            label,
        })
        .await
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
        .call(Fork { volume, label })
        .await
}

#[op2]
#[serde]
pub(super) async fn op_consolestorage_saves(
    state: Rc<RefCell<OpState>>,
    #[serde] volume: VolumeRef,
) -> Result<Vec<StorageSave>, JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .saves
        .call(ListSaves { volume })
        .await
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
        .call(SaveHistory { volume, save })
        .await
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
        .call(RestoreSave {
            volume,
            save,
            commit,
        })
        .await
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
        .call(CopySave {
            volume,
            save,
            destination,
        })
        .await
}

#[op2]
pub(super) async fn op_consolestorage_save_delete(
    state: Rc<RefCell<OpState>>,
    #[serde] volume: VolumeRef,
    #[string] save: String,
) -> Result<(), JsErrorBox> {
    ConsoleStorage::from_state(&state)?
        .delete_save
        .call(DeleteSave { volume, save })
        .await
}
