use std::{collections::BTreeMap, sync::Arc};

use consolespec::MachineSpec;
use serde_json::Value;
use snowglass_consolestorage::savegame::{Package, SaveGameSink};
use snowglass_library::{ConsoleStorage, StorageManager};
use snowglass_pluginhost::StorageFactories;
use snowglass_vfs::{MemoryCardStorageFactory, NandVolumeFactory};
use uuid::Uuid;

fn is_setup_launch(export_path: &[String]) -> bool {
    matches!(export_path, [name] if name == "setupLaunch")
        || matches!(export_path, [default, name] if default == "default" && name == "setupLaunch")
}

pub async fn launch_storage(
    manager: Option<Arc<StorageManager>>,
    export_path: &[String],
    arguments: &[Value],
) -> Result<StorageFactories, String> {
    if !is_setup_launch(export_path) {
        return Ok(BTreeMap::new());
    }

    let Some(arguments) = arguments.first() else {
        return Ok(BTreeMap::new());
    };
    let Some(requested) = arguments.get("storage") else {
        return Ok(BTreeMap::new());
    };
    let requested = requested
        .as_object()
        .ok_or_else(|| "launch storage must be an object".to_owned())?;
    if requested.is_empty() {
        return Ok(BTreeMap::new());
    }

    let console = arguments
        .get("console")
        .and_then(Value::as_str)
        .ok_or_else(|| "launch storage requires a string console".to_owned())?;
    let machine = MachineSpec::try_from(console).map_err(|error| error.to_string())?;
    let declared = machine
        .storage_devices()
        .map(|device| device.id())
        .collect::<std::collections::BTreeSet<_>>();
    let manager = manager.ok_or_else(|| "console storage is unavailable".to_owned())?;
    let package = arguments.get("package").map(|package| {
        Package::new(
            package
                .get("serial")
                .and_then(Value::as_str)
                .map(str::to_owned),
            package
                .get("titleId")
                .and_then(Value::as_str)
                .map(str::to_owned),
        )
    });

    let mut storage = StorageFactories::new();
    for (name, id) in requested {
        if !declared.contains(name.as_str()) {
            return Err(format!(
                "storage device '{name}' is not declared by console '{console}'"
            ));
        }
        let id = id
            .as_str()
            .ok_or_else(|| format!("storage device '{name}' requires a string id"))?;
        let id = Uuid::parse_str(id)
            .map_err(|_| format!("`{id}` is not a console storage id for '{name}'"))?;
        let volume = manager
            .open(console, id)
            .await
            .map_err(|error| error.to_string())?;
        match volume {
            ConsoleStorage::Nand(nand) => {
                storage.insert(name.clone(), Arc::new(NandVolumeFactory::new(nand)));
            }
            ConsoleStorage::DreamcastMemoryCard(card) => {
                let package = package
                    .as_ref()
                    .ok_or_else(|| "Dreamcast memory cards require a launch package".to_owned())?;
                if package.serial().is_none() {
                    return Err(
                        "Dreamcast package identity does not contain a product ID".to_owned()
                    );
                }
                let primary = vec![card.infer_save_depot(package)];
                storage.insert(
                    name.clone(),
                    Arc::new(MemoryCardStorageFactory::new(card, primary)),
                );
            }
            ConsoleStorage::GameCubeMemoryCard(card) => {
                let package = package
                    .as_ref()
                    .ok_or_else(|| "GameCube memory cards require a launch package".to_owned())?;
                if package.serial().is_none() {
                    return Err(
                        "GameCube package identity does not contain a product code".to_owned()
                    );
                }
                let primary = vec![card.infer_save_depot(package)];
                storage.insert(
                    name.clone(),
                    Arc::new(MemoryCardStorageFactory::new(card, primary)),
                );
            }
            ConsoleStorage::PlaystationMemoryCard(card) => {
                let package = package.as_ref().ok_or_else(|| {
                    "PlayStation memory cards require a launch package".to_owned()
                })?;
                if package.serial().is_none() {
                    return Err(
                        "PlayStation package identity does not contain a product serial".to_owned(),
                    );
                }
                let primary = vec![card.infer_save_depot(package)];
                storage.insert(
                    name.clone(),
                    Arc::new(MemoryCardStorageFactory::new(card, primary)),
                );
            }
            ConsoleStorage::Playstation2MemoryCard(card) => {
                let package = package.as_ref().ok_or_else(|| {
                    "PlayStation 2 memory cards require a launch package".to_owned()
                })?;
                if package.serial().is_none() {
                    return Err(
                        "PlayStation 2 package identity does not contain a product serial"
                            .to_owned(),
                    );
                }
                let primary = vec![card.infer_save_depot(package)];
                storage.insert(
                    name.clone(),
                    Arc::new(MemoryCardStorageFactory::new(card, primary)),
                );
            }
        }
    }

    Ok(storage)
}
