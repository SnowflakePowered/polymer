use super::Device;
use futures_util::StreamExt;
use polymer_library::StorageManager;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepotSource {
    pub path: String,
    #[serde(default)]
    pub cache: Option<String>,
}

impl DepotSource {
    pub async fn install(
        &self,
        device: &Device,
        manager: &StorageManager,
        path: &Path,
        progress: impl Fn(String),
    ) -> Result<Value, String> {
        let nand = device.open(manager).await?;
        let key_root = manager.root().join("keys");
        let installation = nand.try_install(path).map_err(|error| error.to_string())?;
        let records = installation.embedded_keys();
        if !records.is_empty() {
            use polymer_consolestorage::keys::KeyWriter;
            let store = polymer_library::keys::KeyStore::open(key_root)
                .await
                .map_err(|error| error.to_string())?;
            store
                .for_platform(&device.platform)
                .map_err(|error| error.to_string())?
                .put(records)
                .await
                .map_err(|error| error.to_string())?;
        }
        Self::finish_install(installation, progress).await
    }

    pub async fn finish_install(
        mut installation: polymer_consolestorage::installable::Installation<'_>,
        progress: impl Fn(String),
    ) -> Result<Value, String> {
        while let Some(item) = installation.next().await {
            progress(item.map_err(|error| error.to_string())?);
        }
        let artifact = installation
            .finish()
            .await
            .map_err(|error| error.to_string())?;
        Ok(Device::artifact(&artifact))
    }
}
