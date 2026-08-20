use std::sync::Arc;

use serde_json::{Value, json};
use snowglass_deno::snowglass::ConsoleStorageRequest;
use snowglass_library::{Directories, Nand, StorageManager};
use uuid::Uuid;

pub(crate) fn manager() -> Option<Arc<StorageManager>> {
    Directories::discover()
        .ok()
        .map(|directories| Arc::new(StorageManager::new(directories.consolestorage())))
}

pub(crate) async fn serve(
    manager: Option<Arc<StorageManager>>,
    request: ConsoleStorageRequest,
) -> Result<Value, String> {
    let manager = manager.ok_or_else(|| "console storage is unavailable".to_owned())?;

    match request {
        ConsoleStorageRequest::List => {
            let nands = manager.list().await.map_err(|error| error.to_string())?;
            Ok(Value::Array(nands.iter().map(describe).collect()))
        }
        ConsoleStorageRequest::Create { platform, label } => manager
            .create(&platform, label)
            .await
            .map(|nand| describe(&nand))
            .map_err(|error| error.to_string()),
        ConsoleStorageRequest::Delete { platform, id } => manager
            .delete(&platform, parse(&id)?)
            .await
            .map(|()| Value::Null)
            .map_err(|error| error.to_string()),
        ConsoleStorageRequest::SetLabel {
            platform,
            id,
            label,
        } => manager
            .set_label(&platform, parse(&id)?, label)
            .await
            .map(|nand| describe(&nand))
            .map_err(|error| error.to_string()),
    }
}

fn parse(id: &str) -> Result<Uuid, String> {
    Uuid::parse_str(id).map_err(|_| format!("`{id}` is not a nand id"))
}

fn describe(nand: &Nand) -> Value {
    json!({
        "id": nand.metadata.id.simple().to_string(),
        "platform": nand.metadata.platform,
        "label": nand.metadata.label,
        "created": nand.metadata.created.to_string(),
        "path": nand.path.to_string_lossy(),
    })
}
