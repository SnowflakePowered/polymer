use std::sync::Arc;

use serde_json::{Value, json};
use snowglass_deno::snowglass::ConsoleStorageRequest;
use snowglass_library::{Directories, NandMetadata, StorageManager, VirtualNand};
use uuid::Uuid;

pub fn manager() -> Option<Arc<StorageManager>> {
    Directories::discover()
        .ok()
        .map(|directories| Arc::new(StorageManager::new(directories.consolestorage())))
}

pub async fn serve(
    manager: Option<Arc<StorageManager>>,
    request: ConsoleStorageRequest,
) -> Result<Value, String> {
    let manager = manager.ok_or_else(|| "console storage is unavailable".to_owned())?;

    match request {
        ConsoleStorageRequest::List => {
            let nands = manager.list().await.map_err(|error| error.to_string())?;
            Ok(Value::Array(
                nands
                    .iter()
                    .map(|metadata| describe(&manager, metadata))
                    .collect(),
            ))
        }
        ConsoleStorageRequest::Create { platform, label } => {
            let nand = manager
                .create(&platform, label)
                .await
                .map_err(|error| error.to_string())?;
            let metadata = nand.metadata().await.map_err(|error| error.to_string())?;
            Ok(describe(&manager, &metadata))
        }
        ConsoleStorageRequest::Delete { platform, id } => manager
            .delete(&platform, parse(&id)?)
            .await
            .map(|()| Value::Null)
            .map_err(|error| error.to_string()),
        ConsoleStorageRequest::SetLabel {
            platform,
            id,
            label,
        } => {
            let nand = manager
                .set_label(&platform, parse(&id)?, label)
                .await
                .map_err(|error| error.to_string())?;
            let metadata = nand.metadata().await.map_err(|error| error.to_string())?;
            Ok(describe(&manager, &metadata))
        }
    }
}

fn parse(id: &str) -> Result<Uuid, String> {
    Uuid::parse_str(id).map_err(|_| format!("`{id}` is not a nand id"))
}

fn describe(manager: &StorageManager, metadata: &NandMetadata) -> Value {
    json!({
        "id": metadata.id.simple().to_string(),
        "platform": metadata.platform,
        "label": metadata.label,
        "created": metadata.created.to_string(),
        "path": manager
            .path(&metadata.platform, metadata.id)
            .to_string_lossy(),
    })
}
