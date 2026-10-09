use std::{collections::BTreeMap, sync::Arc};

use consolespec::MachineSpec;
use parking_lot::Mutex;
use polymer_consolestorage::impls::dreamcast::DreamcastMemoryCard;
use polymer_consolestorage::impls::gamecube::GameCubeMemoryCard;
use polymer_consolestorage::impls::ps1::PlaystationMemoryCard;
use polymer_consolestorage::impls::ps2::Playstation2MemoryCard;
use polymer_consolestorage::savegame::{Package, SaveGameSink};
use polymer_consolestorage::{
    BatteryIdentity, BatterySaveRepo, CheckpointGate, CheckpointRequest, SaveRepo,
};
use polymer_deno::polymer::{PolymerNotification, PolymerNotificationSender};
use polymer_library::keys::KeyStore;
use polymer_library::{
    ConsoleStorage, InstalledArtifact, LaunchableUri, StorageKind, StorageManager,
};
use polymer_pluginhost::controller::{
    ControllerClient, SaveCheckpoint, SaveCheckpointRequest, SaveCheckpointSave,
};
use polymer_pluginhost::filesystem::StorageFactories;
use polymer_vfs::{
    BatteryStorageFactory, FinalizationReason, MaterializationScope, MemoryCardStorageFactory,
    NandVolumeFactory, ResourceFactory, ResourceFinalizer, VfsError, VfsResult, Volume,
};
use serde_json::Value;
use uuid::Uuid;

const SAVE_CHECKPOINT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// The plugin session asked before mid-session save checkpoints. It is attached once the
/// emulator's controller session exists; until then, checkpoints proceed without asking.
#[derive(Clone, Default)]
pub struct CheckpointSession(Arc<Mutex<Option<ControllerClient>>>);

impl CheckpointSession {
    pub fn attach(&self, client: ControllerClient) {
        *self.0.lock() = Some(client);
    }

    fn gate(&self, storage: String) -> CheckpointGate {
        let session = self.clone();
        Box::new(move |request: CheckpointRequest| {
            let Some(client) = session.0.lock().clone() else {
                return Box::pin(async { true });
            };
            let message = SaveCheckpoint {
                request: SaveCheckpointRequest {
                    storage: storage.clone(),
                    save: SaveCheckpointSave {
                        id: format!("{:016x}", request.save.0),
                        path: request.root,
                    },
                    changed: request.changed,
                },
                timeout: SAVE_CHECKPOINT_TIMEOUT,
            };
            Box::pin(async move {
                match client.send(message).await {
                    Ok(Ok(ready)) => ready,
                    Ok(Err(error)) => {
                        tracing::warn!(%error, "onSaveCheckpoint failed; checkpointing anyway");
                        true
                    }
                    Err(_) => true,
                }
            })
        })
    }
}

struct SaveWatchFactory {
    inner: Arc<dyn ResourceFactory<dyn Volume>>,
    saves: Arc<dyn SaveRepo>,
    notification: GameSavedNotification,
    storage: String,
    checkpoints: CheckpointSession,
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
    fn materialize_in(&self, scope: &mut MaterializationScope) -> VfsResult<Arc<dyn Volume>> {
        let watcher = self.saves.watch_saves(&[])?;
        let baseline_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;
        let backend = scope.build(&*self.inner)?;
        let baseline_saves = self.saves.clone();
        let baseline = std::thread::Builder::new()
            .name("polymer-save-baseline".to_owned())
            .spawn(move || baseline_runtime.block_on(baseline_saves.checkpoint_saves()))?;
        baseline
            .join()
            .map_err(|_| VfsError::Unsupported("save history baseline panicked".to_owned()))??;
        let saves = self.saves.clone();
        let notification = self.notification.clone();
        let gate = self.checkpoints.gate(self.storage.clone());
        let (stop, mut stopped) = tokio::sync::mpsc::unbounded_channel::<
            std::sync::mpsc::SyncSender<Result<(), polymer_consolestorage::Error>>,
        >();
        let thread = std::thread::Builder::new()
            .name("polymer-save-history".to_owned())
            .spawn(move || {
                runtime.block_on(async move {
                    loop {
                        tokio::select! {
                            request = stopped.recv() => {
                                let result = saves.checkpoint_saves().await;
                                if matches!(result, Ok(true)) {
                                    notification.publish();
                                }
                                let finished = result.is_ok();
                                if let Some(reply) = request {
                                    let _ = reply.send(result.map(|_| ()));
                                } else {
                                    result?;
                                    return Ok(());
                                }
                                if finished {
                                    return Ok(());
                                }
                            },
                            _ = watcher.changed() => {
                                if checkpoint_saves_logged(&*saves, &gate).await {
                                    notification.publish();
                                }
                            },
                        }
                    }
                })
            })?;
        scope.push(Arc::new(SaveWatchingLease {
            task: Mutex::new(SaveWatchingTask {
                stop,
                thread: Some(thread),
            }),
        }));
        Ok(backend)
    }
}

struct SaveWatchingLease {
    task: Mutex<SaveWatchingTask>,
}

struct SaveWatchingTask {
    stop: tokio::sync::mpsc::UnboundedSender<
        std::sync::mpsc::SyncSender<Result<(), polymer_consolestorage::Error>>,
    >,
    thread: Option<std::thread::JoinHandle<Result<(), polymer_consolestorage::Error>>>,
}

impl SaveWatchingLease {
    fn stop(&self) -> VfsResult<()> {
        let mut task = self.task.lock();
        if task.thread.is_none() {
            return Ok(());
        }
        let (finished, result) = std::sync::mpsc::sync_channel(1);
        task.stop
            .send(finished)
            .map_err(|_| VfsError::Unsupported("save history watcher stopped".to_owned()))?;
        result
            .recv()
            .map_err(|_| VfsError::Unsupported("save history watcher stopped".to_owned()))?
            .map_err(VfsError::from)?;
        task.thread
            .take()
            .expect("the stop caller owns the history thread")
            .join()
            .map_err(|_| VfsError::Unsupported("save history watcher panicked".to_owned()))?
            .map_err(VfsError::from)
    }
}

impl ResourceFinalizer for SaveWatchingLease {
    fn finalize(&self, _reason: FinalizationReason) -> VfsResult<()> {
        self.stop()
    }
}

impl Drop for SaveWatchingLease {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

async fn checkpoint_saves_logged(saves: &dyn SaveRepo, gate: &CheckpointGate) -> bool {
    match saves.checkpoint_saves_gated(Some(gate)).await {
        Ok(changed) => changed,
        Err(error) => {
            tracing::error!(%error, "failed to checkpoint console save history");
            false
        }
    }
}

fn watch_save_history(
    factory: Arc<dyn ResourceFactory<dyn Volume>>,
    saves: Arc<dyn SaveRepo>,
    notification: GameSavedNotification,
    storage: &str,
    checkpoints: &CheckpointSession,
) -> Arc<dyn ResourceFactory<dyn Volume>> {
    Arc::new(SaveWatchFactory {
        inner: factory,
        saves,
        notification,
        storage: storage.to_owned(),
        checkpoints: checkpoints.clone(),
    })
}

fn is_emulator_launch(export_path: &[String]) -> bool {
    matches!(export_path, [name] if name == "onLaunch")
        || matches!(export_path, [default, name] if default == "default" && name == "onLaunch")
}

async fn select_artifact(
    manager: &StorageManager,
    console: &str,
    requested: &mut serde_json::Map<String, Value>,
    uri: Option<&LaunchableUri>,
) -> Result<(), String> {
    let Some(LaunchableUri::Artifact(InstalledArtifact {
        platform, storage, ..
    })) = uri
    else {
        return Ok(());
    };
    let Some((device, StorageKind::Nand)) = ConsoleStorage::default_device(platform) else {
        return Err(format!("{platform} has no NAND for installed titles"));
    };
    let saves = match requested
        .get(device)
        .and_then(Value::as_array)
        .and_then(|selection| selection.first())
    {
        Some(saves) => saves.clone(),
        None => manager
            .default_selection(StorageKind::Nand, console)
            .await
            .map_err(|error| error.to_string())?[0]
            .clone(),
    };
    requested.insert(
        device.to_owned(),
        serde_json::json!([saves, storage.to_string()]),
    );
    Ok(())
}

pub async fn launch_storage(
    manager: Option<Arc<StorageManager>>,
    export_path: &[String],
    arguments: &[Value],
    notifications: PolymerNotificationSender,
    checkpoints: &CheckpointSession,
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
    let default_device = ConsoleStorage::default_device(console);
    if requested.is_none_or(serde_json::Map::is_empty)
        && !battery_supported
        && default_device.is_none()
    {
        return Ok(BTreeMap::new());
    }
    let machine = MachineSpec::try_from(console).map_err(|error| error.to_string())?;
    let declared = machine
        .storage_devices()
        .map(|device| device.id())
        .collect::<std::collections::BTreeSet<_>>();
    let manager = manager.ok_or_else(|| "console storage is unavailable".to_owned())?;
    let initial = arguments
        .get("initialDiscIndex")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    let metadata = arguments
        .get("launchSet")
        .and_then(|set| set.get(initial))
        .and_then(|disc| disc.get("package"))
        .map(|value| serde_json::from_value::<polymer_scraping::Package>(value.clone()))
        .transpose()
        .map_err(|error| error.to_string())?;
    let mut requested = requested.cloned().unwrap_or_default();
    select_artifact(
        &manager,
        console,
        &mut requested,
        metadata.as_ref().map(|package| &package.uri),
    )
    .await?;
    if let Some((device, kind)) = default_device
        && !requested.contains_key(device)
    {
        let selection = manager
            .default_selection(kind, console)
            .await
            .map_err(|error| error.to_string())?;
        requested.insert(device.to_owned(), selection);
    }
    let key_store = KeyStore::open(manager.root().join("keys"))
        .await
        .map_err(|e| e.to_string())?;
    let keys = Arc::new(key_store.for_platform(console).map_err(|e| e.to_string())?);
    let package = metadata.as_ref().map(|package| {
        Package::new(package.serial.clone(), package.title_id.clone())
            .with_binary_digest(package.binary_digest.clone())
    });

    let mut storage = StorageFactories::new();
    if battery_supported {
        let selection = requested.get("battery");
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
                let title = metadata
                    .as_ref()
                    .map(|package| package.filename.as_str())
                    .filter(|title| !title.is_empty())
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
                Arc::new(BatteryStorageFactory::new(battery, id)),
                saves,
                GameSavedNotification {
                    sender: notifications.clone(),
                    kind: StorageKind::Battery,
                    platform: console.to_owned(),
                    id,
                },
                "battery",
                checkpoints,
            ),
        );
    }
    for (name, selection) in &requested {
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
            if let (Some(package), Some(path)) = (
                package.as_ref(),
                metadata
                    .as_ref()
                    .and_then(|metadata| metadata.uri.binary_path().ok()),
            ) && let Err(error) = save.describe_launch(package, &path).await
            {
                tracing::warn!(%error, "could not describe saves from the launched game");
            }
            let composed = nand
                .compose(Some(save.clone()), keys.clone())
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
                    GameSavedNotification {
                        sender: notifications.clone(),
                        kind: StorageKind::SaveRepo,
                        platform: console.to_owned(),
                        id: save_id,
                    },
                    name,
                    checkpoints,
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
                let root = nand
                    .compose(None, keys.clone())
                    .await
                    .map_err(|e| e.to_string())?;
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
                                name,
                                checkpoints,
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
