use std::sync::Arc;

use serde_json::Value;
use snowglass_library::StorageManager;
use snowglass_vfs::{NandVolumeFactory, ResourceFactory, Volume};
use uuid::Uuid;

pub type NandFactory = Arc<dyn ResourceFactory<dyn Volume>>;

fn is_setup_launch(export_path: &[String]) -> bool {
    matches!(export_path, [name] if name == "setupLaunch")
        || matches!(export_path, [default, name] if default == "default" && name == "setupLaunch")
}

pub async fn launch_nand(
    manager: Option<Arc<StorageManager>>,
    export_path: &[String],
    arguments: &[Value],
) -> Result<Option<NandFactory>, String> {
    if !is_setup_launch(export_path) {
        return Ok(None);
    }

    let Some(request) = arguments.first().and_then(|arguments| arguments.get("nand")) else {
        return Ok(None);
    };

    let platform = request
        .get("platform")
        .and_then(Value::as_str)
        .ok_or_else(|| "launch nand requires a string platform".to_owned())?;
    let id = request
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "launch nand requires a string id".to_owned())?;
    let id = Uuid::parse_str(id).map_err(|_| format!("`{id}` is not a nand id"))?;

    let manager = manager.ok_or_else(|| "console storage is unavailable".to_owned())?;
    let nand = manager
        .open(platform, id)
        .await
        .map_err(|error| error.to_string())?;

    Ok(Some(Arc::new(NandVolumeFactory::new(Arc::new(nand)))))
}
