use polymer_consolestorage::{
    ConsoleStorage, StorageKind,
    nand::{NandCompletion, VirtualNand},
};
use polymer_library::{StorageInspection, StorageManager};
use serde::{Deserialize, Serialize};
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

    pub async fn inspect(&self, manager: &StorageManager) -> Result<StorageInspection, String> {
        let nand = self.open(manager).await?;
        Ok(StorageInspection {
            id: self.id.simple().to_string(),
            platform: nand.platform().into(),
            minimal: nand.is_complete(NandCompletion::Minimal),
            dirty: nand.is_dirty().map_err(|error| error.to_string())?,
            artifacts: nand.artifacts().await.map_err(|error| error.to_string())?,
        })
    }
}
