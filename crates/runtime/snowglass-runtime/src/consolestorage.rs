use std::sync::Arc;

use serde_json::{Value, json};
use snowglass_deno::snowglass::ConsoleStorageRequest;
use snowglass_library::{Directories, StorageKind, StorageManager, VolumeInfo};
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
            let storage = manager.list().await.map_err(|error| error.to_string())?;
            Ok(Value::Array(
                storage
                    .iter()
                    .map(|(kind, metadata)| describe(&manager, *kind, metadata))
                    .collect(),
            ))
        }
        ConsoleStorageRequest::Create {
            kind,
            platform,
            label,
        } => {
            let kind = kind
                .parse::<StorageKind>()
                .map_err(|error| error.to_string())?;
            let storage = manager
                .create(kind, &platform, label)
                .await
                .map_err(|error| error.to_string())?;
            let metadata = storage
                .metadata()
                .await
                .map_err(|error| error.to_string())?;
            Ok(describe(&manager, kind, &metadata))
        }
        ConsoleStorageRequest::Delete { kind, platform, id } => manager
            .delete(
                kind.parse::<StorageKind>()
                    .map_err(|error| error.to_string())?,
                &platform,
                parse(&id)?,
            )
            .await
            .map(|()| Value::Null)
            .map_err(|error| error.to_string()),
        ConsoleStorageRequest::SetLabel {
            kind,
            platform,
            id,
            label,
        } => {
            let kind = kind
                .parse::<StorageKind>()
                .map_err(|error| error.to_string())?;
            let storage = manager
                .set_label(kind, &platform, parse(&id)?, label)
                .await
                .map_err(|error| error.to_string())?;
            let metadata = storage
                .metadata()
                .await
                .map_err(|error| error.to_string())?;
            Ok(describe(&manager, kind, &metadata))
        }
    }
}

fn parse(id: &str) -> Result<Uuid, String> {
    Uuid::parse_str(id).map_err(|_| format!("`{id}` is not a console storage id"))
}

fn describe(manager: &StorageManager, kind: StorageKind, metadata: &VolumeInfo) -> Value {
    json!({
        "kind": kind.as_str(),
        "id": metadata.id.simple().to_string(),
        "platform": metadata.platform,
        "label": metadata.label,
        "created": metadata.created.to_string(),
        "path": manager
            .path(kind, &metadata.platform, metadata.id)
            .to_string_lossy(),
    })
}
