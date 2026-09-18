use std::{collections::BTreeMap, sync::Arc};

use consolespec::MachineSpec;
use parking_lot::Mutex;
use polymer_consolestorage::impls::dreamcast::DreamcastMemoryCard;
use polymer_consolestorage::impls::gamecube::GameCubeMemoryCard;
use polymer_consolestorage::impls::ps1::PlaystationMemoryCard;
use polymer_consolestorage::impls::ps2::Playstation2MemoryCard;
use polymer_consolestorage::savegame::{Package, SaveGameSink};
use polymer_consolestorage::{BatteryIdentity, BatterySaveRepo, SaveRepo};
use polymer_deno::polymer::{PolymerNotification, PolymerNotificationSender};
use polymer_library::keys::KeyStore;
use polymer_library::{ConsoleStorage, StorageKind, StorageManager};
use polymer_pluginhost::filesystem::StorageFactories;
use polymer_vfs::{
    BatteryStorageFactory, CompositeResourceLease, ExclusiveResourceKey, FinalizationReason,
    Materialized, MemoryCardStorageFactory, NandVolumeFactory, ResourceFactory, ResourceLease,
    VfsError, VfsResult, Volume,
};
use serde_json::Value;
use uuid::Uuid;

struct SaveWatchFactory {
    inner: Arc<dyn ResourceFactory<dyn Volume>>,
    saves: Arc<dyn SaveRepo>,
    exclusive: Option<Uuid>,
    notification: GameSavedNotification,
}

impl SaveWatchFactory {
    fn new(
        inner: Arc<dyn ResourceFactory<dyn Volume>>,
        saves: Arc<dyn SaveRepo>,
        exclusive: Option<Uuid>,
        notification: GameSavedNotification,
    ) -> Self {
        Self {
            inner,
            saves,
            exclusive,
            notification,
        }
    }
}

#[derive(Clone)]
struct GameSavedNotification {
    sender: PolymerNotificationSender,
    kind: StorageKind,
    platform: String,
    id: Uuid,
}

impl GameSavedNotification {
    fn publish(&self) {
        let notification = PolymerNotification::GameSaved {
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
        let ownership = self
            .exclusive
            .map(|id| ExclusiveResourceKey::Resource(id).acquire())
            .transpose()?;
        // Register before the emulator can access the mount so no save write
        // can race watcher creation.
        let watcher = self.saves.watch_saves(&[]).map_err(console_storage_error)?;
        let baseline_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;
        let materialized = self.inner.materialize()?;
        // Establish a silent baseline before the mounted file becomes visible
        // to the emulator. This also recovers canonical state left uncommitted
        // by an interrupted session.
        let baseline_saves = self.saves.clone();
        let baseline = match std::thread::Builder::new()
            .name("polymer-save-baseline".to_owned())
            .spawn(move || baseline_runtime.block_on(baseline_saves.checkpoint_saves()))
        {
            Ok(baseline) => baseline,
            Err(source) => {
                materialized
                    .lease
                    .finalize(FinalizationReason::LaunchFailed)?;
                return Err(VfsError::Io(source));
            }
        };
        match baseline.join() {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                materialized
                    .lease
                    .finalize(FinalizationReason::LaunchFailed)?;
                return Err(console_storage_error(error));
            }
            Err(_) => {
                materialized
                    .lease
                    .finalize(FinalizationReason::LaunchFailed)?;
                return Err(VfsError::Unsupported(
                    "save history baseline panicked".to_owned(),
                ));
            }
        }
        let saves = self.saves.clone();
        let notification = self.notification.clone();
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let thread = match std::thread::Builder::new()
            .name("polymer-save-history".to_owned())
            .spawn(move || {
                runtime.block_on(async move {
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

        let mut leases = ownership.into_iter().collect::<Vec<_>>();
        leases.push(history_lease);
        leases.push(materialized.lease);
        Ok(Materialized {
            backend: materialized.backend,
            // Composite leases finalize in reverse: flush the mounted storage,
            // stop the watcher after one final synchronous checkpoint, then
            // release any outer profile lock.
            lease: CompositeResourceLease::new(leases),
        })
    }
}

struct SaveWatchingLease {
    task: Mutex<SaveWatchingTask>,
}

struct SaveWatchingTask {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<Result<(), polymer_consolestorage::Error>>>,
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

fn console_storage_error(error: polymer_consolestorage::Error) -> VfsError {
    match error {
        polymer_consolestorage::Error::Io { source, .. } => VfsError::Io(source),
        error => VfsError::Unsupported(error.to_string()),
    }
}

fn watch_save_history(
    factory: Arc<dyn ResourceFactory<dyn Volume>>,
    saves: Arc<dyn SaveRepo>,
    exclusive: Option<Uuid>,
    notification: GameSavedNotification,
) -> Arc<dyn ResourceFactory<dyn Volume>> {
    Arc::new(SaveWatchFactory::new(
        factory,
        saves,
        exclusive,
        notification,
    ))
}

fn is_emulator_launch(export_path: &[String]) -> bool {
    matches!(export_path, [name] if name == "onLaunch")
        || matches!(export_path, [default, name] if default == "default" && name == "onLaunch")
}

pub async fn launch_storage(
    manager: Option<Arc<StorageManager>>,
    export_path: &[String],
    arguments: &[Value],
    notifications: PolymerNotificationSender,
) -> Result<StorageFactories, String> {
    if !is_emulator_launch(export_path) {
        return Ok(BTreeMap::new());
    }

    let Some(arguments) = arguments.first() else {
        return Ok(BTreeMap::new());
    };
    let requested = arguments
        .get("storage")
        .map(|storage| {
            storage
                .as_object()
                .ok_or_else(|| "launch storage must be an object".to_owned())
        })
        .transpose()?;
    let Some(console) = arguments.get("console").and_then(Value::as_str) else {
        if requested.is_none_or(serde_json::Map::is_empty) {
            return Ok(BTreeMap::new());
        }
        return Err("launch storage requires a string console".to_owned());
    };
    let battery_supported = BatterySaveRepo::supports(console);
    if requested.is_none_or(serde_json::Map::is_empty) && !battery_supported {
        return Ok(BTreeMap::new());
    }
    let machine = MachineSpec::try_from(console).map_err(|error| error.to_string())?;
    let declared = machine
        .storage_devices()
        .map(|device| device.id())
        .collect::<std::collections::BTreeSet<_>>();
    let manager = manager.ok_or_else(|| "console storage is unavailable".to_owned())?;
    let key_store = KeyStore::open(manager.root().join("keys"))
        .await
        .map_err(|e| e.to_string())?;
    let keys = key_store.for_platform(console).map_err(|e| e.to_string())?;
    let metadata = arguments
        .get("package")
        .map(|value| serde_json::from_value::<polymer_scraping::Package>(value.clone()))
        .transpose()
        .map_err(|error| error.to_string())?;
    let package = metadata.as_ref().map(|package| {
        Package::new(package.serial.clone(), package.title_id.clone())
            .with_binary_digest(package.binary_digest.clone())
    });

    let mut storage = StorageFactories::new();
    if battery_supported {
        let selection = requested.and_then(|requested| requested.get("battery"));
        let (id, volume) = match selection {
            Some(selection) => {
                let id = parse_id("battery", selection)?;
                let volume = manager
                    .open(StorageKind::Battery, console, id)
                    .await
                    .map_err(|error| error.to_string())?;
                (id, volume)
            }
            None => {
                let package = package
                    .as_ref()
                    .ok_or_else(|| format!("{console} battery saves require a launch package"))?;
                let rom = arguments
                    .get("launchPath")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("{console} battery saves require a launch path"))?;
                let title = metadata
                    .as_ref()
                    .map(|package| package.filename.as_str())
                    .filter(|title| !title.is_empty())
                    .or_else(|| {
                        std::path::Path::new(rom)
                            .file_stem()
                            .and_then(|name| name.to_str())
                    })
                    .unwrap_or("Battery save");
                let identity = BatteryIdentity::from_package(console, title, package)
                    .map_err(|error| error.to_string())?;
                let id = identity.profile_id();
                let volume = manager
                    .open_battery(console, id, Some(identity.title().to_owned()))
                    .await
                    .map_err(|error| error.to_string())?;
                (id, volume)
            }
        };
        let battery = volume
            .downcast::<BatterySaveRepo>()
            .ok_or_else(|| format!("storage `{id}` is not a {console} battery save profile"))?;
        let saves: Arc<dyn SaveRepo> = battery.clone();
        storage.insert(
            "battery".to_owned(),
            watch_save_history(
                Arc::new(BatteryStorageFactory::with_managed_lease(battery)),
                saves,
                Some(id),
                GameSavedNotification {
                    sender: notifications.clone(),
                    kind: StorageKind::Battery,
                    platform: console.to_owned(),
                    id,
                },
            ),
        );
    }
    for (name, selection) in requested.into_iter().flatten() {
        if name == "battery" && battery_supported {
            continue;
        }
        if !declared.contains(name.as_str()) {
            return Err(format!(
                "storage device '{name}' is not declared by console '{console}'"
            ));
        }
        let resolved = polymer_library::StorageSelection::resolve(&manager, console, selection)
            .await
            .map_err(|error| error.to_string())?;
        if !resolved.missing.is_empty() {
            return Err(format!(
                "storage device '{name}' references a missing volume"
            ));
        }
        if let (Some(save_id), Some(nand_id)) = (resolved.saves, resolved.nand) {
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
                .compose(Some(save.clone()), &keys)
                .await
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
                    None,
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

        let (kind, id) = [
            (StorageKind::Nand, resolved.nand),
            (StorageKind::SaveRepo, resolved.saves),
            (StorageKind::MemoryCard, resolved.memory_card),
            (StorageKind::Battery, resolved.battery),
        ]
        .into_iter()
        .find_map(|(kind, id)| id.map(|id| (kind, id)))
        .ok_or_else(|| format!("storage device '{name}' has no volume"))?;
        let volume = manager
            .open(kind, console, id)
            .await
            .map_err(|error| error.to_string())?;
        match volume {
            ConsoleStorage::Nand(nand) => {
                let root = nand.compose(None, &keys).await.map_err(|e| e.to_string())?;
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
                                None,
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
            ConsoleStorage::Battery(_) => {
                return Err(format!(
                    "battery save profile `{id}` must use the reserved 'battery' key"
                ));
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
