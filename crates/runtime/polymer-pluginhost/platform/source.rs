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
        let mut installation = nand.try_install(path).map_err(|error| error.to_string())?;
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
