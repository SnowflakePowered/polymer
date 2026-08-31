use std::{collections::BTreeMap, sync::Arc};

use consolespec::MachineSpec;
use parking_lot::Mutex;
use serde_json::Value;
use snowglass_consolestorage::SaveRepo;
use snowglass_consolestorage::impls::dreamcast::DreamcastMemoryCard;
use snowglass_consolestorage::impls::gamecube::GameCubeMemoryCard;
use snowglass_consolestorage::impls::ps1::PlaystationMemoryCard;
use snowglass_consolestorage::impls::ps2::Playstation2MemoryCard;
use snowglass_consolestorage::savegame::{Package, SaveGameSink};
use snowglass_deno::snowglass::{SnowglassNotification, SnowglassNotificationSender};
use snowglass_library::{ConsoleStorage, StorageKind, StorageManager};
use snowglass_pluginhost::StorageFactories;
use snowglass_vfs::{
    CompositeResourceLease, FinalizationReason, Materialized, MemoryCardStorageFactory,
    NandVolumeFactory, ResourceFactory, ResourceLease, VfsError, VfsResult, Volume,
};
use uuid::Uuid;

struct SaveWatchFactory {
    inner: Arc<dyn ResourceFactory<dyn Volume>>,
    saves: Arc<dyn SaveRepo>,
    notification: GameSavedNotification,
}

impl SaveWatchFactory {
    fn new(
        inner: Arc<dyn ResourceFactory<dyn Volume>>,
        saves: Arc<dyn SaveRepo>,
        notification: GameSavedNotification,
    ) -> Self {
        Self {
            inner,
            saves,
            notification,
        }
    }
}

#[derive(Clone)]
struct GameSavedNotification {
    sender: SnowglassNotificationSender,
    kind: StorageKind,
    platform: String,
    id: Uuid,
}

impl GameSavedNotification {
    fn publish(&self) {
        let notification = SnowglassNotification::GameSaved {
            kind: self.kind.as_str().to_owned(),
            platform: self.platform.clone(),
            id: self.id.simple().to_string(),
        };
        if self.sender.send(notification).is_err() {
            tracing::debug!("gamesaved notification receiver is unavailable");
        }
    }
}

impl ResourceFactory<dyn Volume> for SaveWatchFactory {
    fn materialize(&self) -> VfsResult<Materialized<dyn Volume>> {
        // Register before the emulator can access the mount so no save write
        // can race watcher creation.
        let watcher = self.saves.watch_saves(&[]).map_err(console_storage_error)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;
        let materialized = self.inner.materialize()?;
        let saves = self.saves.clone();
        let notification = self.notification.clone();
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let thread = match std::thread::Builder::new()
            .name("snowglass-save-history".to_owned())
            .spawn(move || {
                runtime.block_on(async move {
                    // Reconcile once on mount as well. This recovers a save
                    // left canonical but uncommitted by an interrupted session.
                    checkpoint_saves_logged(&*saves).await;
                    loop {
                        tokio::select! {
                            _ = &mut stopped => {
                                let changed = saves.checkpoint_saves().await?;
                                if changed {
                                    notification.publish();
                                }
                                return Ok(());
                            },
                            _ = watcher.changed() => {
                                if checkpoint_saves_logged(&*saves).await {
                                    notification.publish();
                                }
                            },
                        }
                    }
                })
            }) {
            Ok(thread) => thread,
            Err(source) => {
                materialized
                    .lease
                    .finalize(FinalizationReason::LaunchFailed)?;
                return Err(VfsError::Io(source));
            }
        };
        let history_lease: Arc<dyn ResourceLease> = Arc::new(SaveWatchingLease {
            task: Mutex::new(SaveWatchingTask {
                stop: Some(stop),
                thread: Some(thread),
            }),
        });

        Ok(Materialized {
            backend: materialized.backend,
            // Composite leases finalize in reverse: flush the mounted storage,
            // then stop the watcher after one final synchronous checkpoint.
            lease: CompositeResourceLease::new(vec![history_lease, materialized.lease]),
        })
    }
}

struct SaveWatchingLease {
    task: Mutex<SaveWatchingTask>,
}

struct SaveWatchingTask {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<Result<(), snowglass_consolestorage::Error>>>,
}

impl SaveWatchingLease {
    fn stop(&self) -> VfsResult<()> {
        let (stop, thread) = {
            let mut task = self.task.lock();
            (task.stop.take(), task.thread.take())
        };
        let Some(thread) = thread else {
            return Ok(());
        };
        if let Some(stop) = stop {
            let _ = stop.send(());
        }
        thread
            .join()
            .map_err(|_| VfsError::Unsupported("save history watcher panicked".to_owned()))?
            .map_err(console_storage_error)
    }
}

impl ResourceLease for SaveWatchingLease {
    fn finalize(&self, _reason: FinalizationReason) -> VfsResult<()> {
        self.stop()
    }
}

impl Drop for SaveWatchingLease {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

async fn checkpoint_saves_logged(saves: &dyn SaveRepo) -> bool {
    match saves.checkpoint_saves().await {
        Ok(changed) => changed,
        Err(error) => {
            tracing::error!(%error, "failed to checkpoint console save history");
            false
        }
    }
}

fn console_storage_error(error: snowglass_consolestorage::Error) -> VfsError {
    match error {
        snowglass_consolestorage::Error::Io { source, .. } => VfsError::Io(source),
        error => VfsError::Unsupported(error.to_string()),
    }
}

fn watch_save_history(
    factory: Arc<dyn ResourceFactory<dyn Volume>>,
    saves: Arc<dyn SaveRepo>,
    notification: GameSavedNotification,
) -> Arc<dyn ResourceFactory<dyn Volume>> {
    Arc::new(SaveWatchFactory::new(factory, saves, notification))
}

fn is_emulator_launch(export_path: &[String]) -> bool {
    matches!(export_path, [name] if name == "onLaunch")
        || matches!(export_path, [default, name] if default == "default" && name == "onLaunch")
}

pub async fn launch_storage(
    manager: Option<Arc<StorageManager>>,
    export_path: &[String],
    arguments: &[Value],
    notifications: SnowglassNotificationSender,
) -> Result<StorageFactories, String> {
    if !is_emulator_launch(export_path) {
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
            let composed = nand
                .compose(save.clone())
                .map_err(|error| error.to_string())?;
            storage.insert(
                name.clone(),
                watch_save_history(
                    Arc::new(NandVolumeFactory::new(
                        nand,
                        composed,
                        vec![save_id, nand_id],
                    )),
                    save,
                    GameSavedNotification {
                        sender: notifications.clone(),
                        kind: StorageKind::SaveRepo,
                        platform: console.to_owned(),
                        id: save_id,
                    },
                ),
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
                storage.insert(
                    name.clone(),
                    Arc::new(NandVolumeFactory::new(nand, root, vec![id])),
                );
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
                        let saves: Arc<dyn SaveRepo> = card.clone();
                        storage.insert(
                            name.clone(),
                            watch_save_history(
                                Arc::new(MemoryCardStorageFactory::new(card, primary, id)),
                                saves,
                                GameSavedNotification {
                                    sender: notifications.clone(),
                                    kind: StorageKind::MemoryCard,
                                    platform: console.to_owned(),
                                    id,
                                },
                            ),
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
    use snowglass_consolestorage::Save;
    use snowglass_consolestorage::installable::artifacts::Artifact;
    use snowglass_consolestorage::installable::{DepotType, IconHint};
    use snowglass_consolestorage::savegame::SaveDepotId;
    use snowglass_library::StorageKind;
    use snowglass_vfs::{
        CreateOptions, FilesystemPlan, FinalizationReason, OpenOptions, Permissions, Resource,
        ResourceOrigin, VfsError,
    };
    use std::io::Read;

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
        let file = nand.root().create("usr/title/immutable.bin").unwrap();
        assert_eq!(file.write_at(0, b"installed").unwrap(), 9);
        nand.record(&Artifact::new(
            "test-title".to_owned(),
            DepotType::App,
            Vec::new(),
            "Test title".to_owned(),
            None,
            IconHint::Builtin(DepotType::App),
        ))
        .await
        .unwrap();

        let arguments = serde_json::json!({
            "console": "NINTENDO_WIIU",
            "storage": { "mlc": [save_id, nand_id] },
        });
        let (notifications, mut saved) = tokio::sync::mpsc::unbounded_channel();
        let factories = launch_storage(
            Some(manager.clone()),
            &["onLaunch".to_owned()],
            &[arguments],
            notifications,
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

        for directory in [
            "/Mlc/usr/save/00050000",
            "/Mlc/usr/save/00050000/12345678",
            "/Mlc/usr/save/00050000/12345678/user",
            "/Mlc/usr/save/00050000/12345678/user/common",
        ] {
            filesystem.create_dir(&directory.parse().unwrap()).unwrap();
        }
        let save_file = "/Mlc/usr/save/00050000/12345678/user/common/data.bin"
            .parse()
            .unwrap();
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
        let forbidden = "/Mlc/usr/title/forbidden.bin".parse().unwrap();
        assert!(matches!(
            filesystem.create_file(&forbidden, CreateOptions::default()),
            Err(VfsError::PermissionDenied(_))
        ));

        // Finalization flushes the mounted save storage before the host-owned
        // watcher performs its last checkpoint.
        filesystem
            .finalize(FinalizationReason::ProcessExited)
            .unwrap();
        assert!(matches!(
            saved.try_recv().unwrap(),
            SnowglassNotification::GameSaved {
                kind,
                platform,
                id: notified_id,
            } if kind == "saveRepo"
                && platform == "NINTENDO_WIIU"
                && notified_id == save_id.simple().to_string()
        ));
        let ConsoleStorage::SaveRepo(saves) = manager
            .open(StorageKind::SaveRepo, "NINTENDO_WIIU", save_id)
            .await
            .unwrap()
        else {
            unreachable!()
        };
        let mut saved_bytes = Vec::new();
        saves
            .open_read("usr/save/00050000/12345678/user/common/data.bin")
            .unwrap()
            .read_to_end(&mut saved_bytes)
            .unwrap();
        assert_eq!(saved_bytes, b"save");
        let ConsoleStorage::Nand(reopened_nand) = manager
            .open(StorageKind::Nand, "NINTENDO_WIIU", nand_id)
            .await
            .unwrap()
        else {
            unreachable!()
        };
        assert!(
            !reopened_nand
                .root()
                .exists("usr/save/00050000/12345678/user/common/data.bin")
        );
        let history = Save::new(saves.as_ref(), SaveDepotId(0x0005_0000_1234_5678))
            .history()
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(
            history[0].operation,
            snowglass_consolestorage::SaveCommitOperation::InitialImport
        );
    }
}
