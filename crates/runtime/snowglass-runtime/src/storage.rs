use std::{collections::BTreeMap, sync::Arc};

use consolespec::MachineSpec;
use serde_json::Value;
use snowglass_consolestorage::impls::dreamcast::DreamcastMemoryCard;
use snowglass_consolestorage::impls::gamecube::GameCubeMemoryCard;
use snowglass_consolestorage::impls::ps1::PlaystationMemoryCard;
use snowglass_consolestorage::impls::ps2::Playstation2MemoryCard;
use snowglass_consolestorage::savegame::{Package, SaveGameSink};
use snowglass_library::{ConsoleStorage, StorageKind, StorageManager};
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
    for (name, selection) in requested {
        if !declared.contains(name.as_str()) {
            return Err(format!(
                "storage device '{name}' is not declared by console '{console}'"
            ));
        }
        if let Some(ids) = selection.as_array() {
            if ids.len() != 2 {
                return Err(format!("storage device '{name}' requires [save, nand]"));
            }
            let save_id = parse_id(name, &ids[0])?;
            let nand_id = parse_id(name, &ids[1])?;
            let save = manager
                .open(StorageKind::SaveRepo, console, save_id)
                .await
                .map_err(|error| error.to_string())?;
            let nand = manager
                .open(StorageKind::Nand, console, nand_id)
                .await
                .map_err(|error| error.to_string())?;
            let (ConsoleStorage::SaveRepo(save), ConsoleStorage::Nand(nand)) = (save, nand) else {
                return Err(format!("storage device '{name}' requires [save, nand]"));
            };
            let composed = nand.compose(save).map_err(|error| error.to_string())?;
            storage.insert(
                name.clone(),
                Arc::new(NandVolumeFactory::new(nand, composed)),
            );
            continue;
        }

        let id = parse_id(name, selection)?;
        let kind = if console == "NINTENDO_WIIU" {
            StorageKind::Nand
        } else {
            StorageKind::MemoryCard
        };
        let volume = manager
            .open(kind, console, id)
            .await
            .map_err(|error| error.to_string())?;
        match volume {
            ConsoleStorage::Nand(nand) => {
                let root = nand.root();
                storage.insert(name.clone(), Arc::new(NandVolumeFactory::new(nand, root)));
            }
            ConsoleStorage::SaveRepo(_) => {
                return Err(format!(
                    "save repository `{id}` must be composed with a nand for '{name}'"
                ));
            }
            memory_card @ ConsoleStorage::MemoryCard(_) => {
                let package = package
                    .as_ref()
                    .ok_or_else(|| format!("{console} memory cards require a launch package"))?;
                if package.serial().is_none() {
                    return Err(format!(
                        "{console} package identity does not contain a memory-card product ID"
                    ));
                }

                macro_rules! mount_card {
                    ($card:ty) => {{
                        let card = memory_card.downcast::<$card>().ok_or_else(|| {
                            format!("storage `{id}` is not a {console} memory card")
                        })?;
                        let primary = vec![card.infer_save_depot(package)];
                        storage.insert(
                            name.clone(),
                            Arc::new(MemoryCardStorageFactory::new(card, primary)),
                        );
                    }};
                }

                match console {
                    DreamcastMemoryCard::PLATFORM => mount_card!(DreamcastMemoryCard),
                    GameCubeMemoryCard::PLATFORM => mount_card!(GameCubeMemoryCard),
                    PlaystationMemoryCard::PLATFORM => mount_card!(PlaystationMemoryCard),
                    Playstation2MemoryCard::PLATFORM => mount_card!(Playstation2MemoryCard),
                    _ => return Err(format!("unsupported memory-card platform `{console}`")),
                }
            }
        }
    }

    Ok(storage)
}

fn parse_id(name: &str, value: &Value) -> Result<Uuid, String> {
    let id = value
        .as_str()
        .ok_or_else(|| format!("storage device '{name}' requires string ids"))?;
    Uuid::parse_str(id).map_err(|_| format!("`{id}` is not a console storage id for '{name}'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use snowglass_library::StorageKind;
    use snowglass_vfs::{
        CreateOptions, FilesystemPlan, OpenOptions, Permissions, Resource, ResourceOrigin, VfsError,
    };
    use std::io::Write;

    #[tokio::test]
    async fn wiiu_launch_composes_save_before_immutable_nand() {
        let root = tempfile::tempdir().unwrap();
        let manager = Arc::new(StorageManager::new(root.path()));
        let nand = manager
            .create(StorageKind::Nand, "NINTENDO_WIIU", Some("System".into()))
            .await
            .unwrap();
        let save = manager
            .create(StorageKind::SaveRepo, "NINTENDO_WIIU", Some("Saves".into()))
            .await
            .unwrap();
        let nand_id = nand.metadata().await.unwrap().id;
        let save_id = save.metadata().await.unwrap().id;
        let ConsoleStorage::Nand(nand) = nand else {
            unreachable!()
        };
        nand.root()
            .create("usr/title/immutable.bin")
            .unwrap()
            .write_all(b"installed")
            .unwrap();

        let arguments = serde_json::json!({
            "console": "NINTENDO_WIIU",
            "storage": { "mlc": [save_id, nand_id] },
        });
        let factories = launch_storage(
            Some(manager.clone()),
            &["setupLaunch".to_owned()],
            &[arguments],
        )
        .await
        .unwrap();
        let factory = factories.get("mlc").unwrap().clone();
        let resource = Arc::new(Resource::new(
            factory,
            Permissions::ALL,
            ResourceOrigin::Other("test:mlc".into()),
        ));
        let mut plan = FilesystemPlan::default();
        plan.mount_storage("/Mlc".parse().unwrap(), resource, Permissions::ALL, None);
        let filesystem = plan.seal().unwrap();
        filesystem.materialize().unwrap();

        let immutable = "/Mlc/usr/title/immutable.bin".parse().unwrap();
        let mut contents = [0_u8; 9];
        let file = filesystem
            .open(
                &immutable,
                OpenOptions {
                    read: true,
                    ..OpenOptions::default()
                },
            )
            .unwrap();
        assert_eq!(file.read(0, &mut contents).unwrap(), contents.len());
        assert_eq!(&contents, b"installed");

        let save_file = "/Mlc/usr/save/runtime.bin".parse().unwrap();
        filesystem
            .create_file(&save_file, CreateOptions::default())
            .unwrap();
        filesystem
            .open(
                &save_file,
                OpenOptions {
                    read: true,
                    write: true,
                    ..OpenOptions::default()
                },
            )
            .unwrap()
            .write(0, b"save")
            .unwrap();
        assert_eq!(
            std::fs::read(
                manager
                    .path(StorageKind::SaveRepo, "NINTENDO_WIIU", save_id)
                    .join("usr/save/runtime.bin")
            )
            .unwrap(),
            b"save"
        );
        assert!(
            !manager
                .path(StorageKind::Nand, "NINTENDO_WIIU", nand_id)
                .join("usr/save/runtime.bin")
                .exists()
        );

        let forbidden = "/Mlc/usr/title/forbidden.bin".parse().unwrap();
        assert!(matches!(
            filesystem.create_file(&forbidden, CreateOptions::default()),
            Err(VfsError::PermissionDenied(_))
        ));
    }
}
