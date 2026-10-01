use actix::{Message, Recipient};
pub use polymer_library::{ArtifactSummary as StorageArtifact, StorageInspection};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct StorageError(pub String);

impl From<String> for StorageError {
    fn from(error: String) -> Self {
        Self(error)
    }
}

impl From<&str> for StorageError {
    fn from(error: &str) -> Self {
        Self(error.to_owned())
    }
}

impl StorageError {
    pub fn from_display(error: impl std::fmt::Display) -> Self {
        Self(error.to_string())
    }
}

#[derive(Debug, Serialize)]
pub struct StoragePlatform {
    pub platform: String,
    pub plugin: String,
    /// Whether the platform plugin checks for and fetches title updates.
    pub updates: bool,
}

#[derive(Debug, Serialize)]
pub struct StorageRemovalPlan {
    pub requested: Vec<String>,
    pub affected: Vec<String>,
    pub revision: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageSave {
    pub id: String,
    pub title: String,
    pub description: String,
    pub modified: String,
    pub icon: Option<String>,
    pub byte_length: u64,
    pub blocks: Option<u64>,
    pub versions: usize,
}

#[derive(Debug, Serialize)]
pub struct StorageSaveCommit {
    pub id: String,
    pub parent: Option<String>,
    pub timestamp: String,
    pub operation: &'static str,
    pub screenshot: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct VolumeRef {
    pub kind: String,
    pub platform: String,
    pub id: String,
}

#[derive(Debug, Serialize)]
pub struct StorageVolume {
    pub kind: String,
    pub id: String,
    pub platform: String,
    pub label: Option<String>,
    pub created: String,
    pub path: String,
}

/// Typed storage endpoints exposed to Deno without depending on the backend.
#[derive(Clone, Debug)]
pub struct ConsoleStorage {
    pub platforms: Recipient<Platforms>,
    pub inspect: Recipient<Inspect>,
    pub recover: Recipient<Recover>,
    pub install: Recipient<Install>,
    pub bootstrap: Recipient<Bootstrap>,
    pub uninstall: Recipient<Uninstall>,
    pub plan_removal: Recipient<PlanRemoval>,
    pub list: Recipient<List>,
    pub supports: Recipient<Supports>,
    pub create: Recipient<Create>,
    pub delete: Recipient<Delete>,
    pub set_label: Recipient<SetLabel>,
    pub fork: Recipient<Fork>,
    pub saves: Recipient<ListSaves>,
    pub save_history: Recipient<SaveHistory>,
    pub restore_save: Recipient<RestoreSave>,
    pub copy_save: Recipient<CopySave>,
    pub delete_save: Recipient<DeleteSave>,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Vec<StoragePlatform>, StorageError>")]
pub struct Platforms;

#[derive(Debug, Message)]
#[rtype(result = "Result<StorageInspection, StorageError>")]
pub struct Inspect {
    pub platform: String,
    pub id: String,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<StorageInspection, StorageError>")]
pub struct Recover {
    pub platform: String,
    pub id: String,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<StorageArtifact, StorageError>")]
pub struct Install {
    pub platform: String,
    pub id: String,
    pub path: std::path::PathBuf,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<StorageInspection, StorageError>")]
pub struct Bootstrap {
    pub platform: String,
    pub id: String,
    pub settings: Value,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Vec<StorageArtifact>, StorageError>")]
pub struct Uninstall {
    pub platform: String,
    pub id: String,
    pub ids: Vec<String>,
    pub revision: String,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<StorageRemovalPlan, StorageError>")]
pub struct PlanRemoval {
    pub platform: String,
    pub id: String,
    pub ids: Vec<String>,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Vec<StorageVolume>, StorageError>")]
pub struct List;

#[derive(Debug, Message)]
#[rtype(result = "Result<bool, StorageError>")]
pub struct Supports {
    pub kind: String,
    pub platform: String,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<StorageVolume, StorageError>")]
pub struct Create {
    pub kind: String,
    pub platform: String,
    pub label: Option<String>,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<(), StorageError>")]
pub struct Delete {
    pub kind: String,
    pub platform: String,
    pub id: String,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<StorageVolume, StorageError>")]
pub struct SetLabel {
    pub kind: String,
    pub platform: String,
    pub id: String,
    pub label: Option<String>,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<StorageVolume, StorageError>")]
pub struct Fork {
    pub volume: VolumeRef,
    pub label: Option<String>,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Vec<StorageSave>, StorageError>")]
pub struct ListSaves {
    pub volume: VolumeRef,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<Vec<StorageSaveCommit>, StorageError>")]
pub struct SaveHistory {
    pub volume: VolumeRef,
    pub save: String,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<StorageSaveCommit, StorageError>")]
pub struct RestoreSave {
    pub volume: VolumeRef,
    pub save: String,
    pub commit: String,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<StorageSaveCommit, StorageError>")]
pub struct CopySave {
    pub volume: VolumeRef,
    pub save: String,
    pub destination: VolumeRef,
}

#[derive(Debug, Message)]
#[rtype(result = "Result<(), StorageError>")]
pub struct DeleteSave {
    pub volume: VolumeRef,
    pub save: String,
}
