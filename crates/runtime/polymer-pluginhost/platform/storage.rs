use polymer_consolestorage::{
    ConsoleStorage, StorageKind,
    installable::artifacts::Artifact,
    nand::{NandCompletion, VirtualNand},
};
use polymer_library::StorageManager;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub platform: String,
    pub id: Uuid,
}

impl Device {
    pub async fn open(&self, manager: &StorageManager) -> Result<Arc<dyn VirtualNand>, String> {
        if !ConsoleStorage::supports(StorageKind::Nand, &self.platform) {
            return Err("Unsupported NAND platform".into());
        }
        match manager
            .open(StorageKind::Nand, &self.platform, self.id)
            .await
            .map_err(|error| error.to_string())?
        {
            ConsoleStorage::Nand(nand) => Ok(nand),
            _ => Err("device is not a NAND".into()),
        }
    }

    pub async fn inspect(&self, manager: &StorageManager) -> Result<DeviceInspection, String> {
        let nand = self.open(manager).await?;
        Ok(DeviceInspection {
            id: self.id.simple().to_string(),
            platform: nand.platform().into(),
            minimal: nand.is_complete(NandCompletion::Minimal),
            dirty: nand.is_dirty().map_err(|error| error.to_string())?,
            artifacts: Self::artifacts(&*nand).await?,
        })
    }

    pub async fn artifacts(nand: &dyn VirtualNand) -> Result<Value, String> {
        Ok(Value::Array(nand.artifacts().await.map_err(|error| error.to_string())?.into_iter().map(|item| json!({"id": item.id, "label": item.label, "version": item.version, "depotType": item.depot_type, "byteLength": item.byte_length})).collect()))
    }

    pub fn artifact(artifact: &Artifact) -> Value {
        json!({"id": artifact.id(), "label": artifact.label(), "version": artifact.version(), "depotType": artifact.depot_type(), "byteLength": artifact.byte_length()})
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInspection {
    pub id: String,
    pub platform: String,
    pub minimal: bool,
    pub dirty: bool,
    pub artifacts: Value,
}
