mod dolphin;
pub mod gci;
mod raw;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use jiff::Timestamp;
use uuid::Uuid;

use self::dolphin::DolphinMemoryStorage;
use self::gci::{Region, Save};
use crate::impls::GameCubeDiscId;
use crate::impls::memorycard::{MemoryCardStorage, RawMemoryCardImage};
use crate::savegame::{
    Package, SaveBlob, SaveBlobStream, SaveDepot, SaveDepotId, SaveDescriptor, SaveDescriptors,
    SaveGameSink, SaveWatcher, icons::IconSave,
};
use crate::savegroups::TARGET_CAPACITY_PERCENT;
use crate::storage::VolumeOverlay;
use crate::{
    BoxFuture, EntryKind, Error, MemoryCard, MemoryCardView, MountableStorage, SaveRepo,
    SaveVersioning, Storage, VersionedStorage, VolumeStorage,
};

const SAVE_ROOT: &str = "card";

#[derive(Clone)]
pub struct GameCubeMemoryCard {
    storage: Arc<VersionedStorage>,
    id: Uuid,
}

impl VolumeOverlay for GameCubeMemoryCard {
    fn inner(&self) -> &dyn VolumeStorage {
        self.storage.as_ref()
    }
}

#[async_trait::async_trait]
impl SaveRepo for GameCubeMemoryCard {
    fn versioning(&self) -> &SaveVersioning {
        self.storage.versioning()
    }

    fn read_save_icon(
        &self,
        id: SaveDepotId,
    ) -> Result<Option<crate::savegame::icons::SaveIcon>, Error> {
        Ok(self
            .saves()
            .into_iter()
            .find(|save| save.depot_id() == id)
            .and_then(|save| save.read_icon()))
    }
}

impl GameCubeMemoryCard {
    pub const PLATFORM: &str = "NINTENDO_GCN";

    pub async fn new_empty(
        storage: VersionedStorage,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Self, Error> {
        let storage = Arc::new(storage);
        storage.create_dir_all(SAVE_ROOT)?;
        storage.brand(id, Self::PLATFORM, label.as_deref()).await?;
        Ok(Self { storage, id })
    }

    pub async fn open(storage: VersionedStorage) -> Result<Self, Error> {
        let storage = Arc::new(storage);
        let identity = storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;
        if identity.platform != Self::PLATFORM {
            return Err(Error::Unsupported(format!(
                "`{storage:?}` is a {} volume",
                identity.platform
            )));
        }
        storage.create_dir_all(SAVE_ROOT)?;
        Ok(Self {
            storage,
            id: identity.id,
        })
    }

    fn saves(&self) -> Vec<Save<'static>> {
        let Ok(entries) = self.storage.read_dir(SAVE_ROOT, false) else {
            return Vec::new();
        };
        let mut saves = entries
            .filter(|entry| {
                entry.metadata.kind == EntryKind::File
                    && entry.name.to_ascii_lowercase().ends_with(".gci")
            })
            .filter_map(|entry| {
                let mut bytes = Vec::new();
                self.storage
                    .open_read(&entry.path)
                    .ok()?
                    .read_to_end(&mut bytes)
                    .ok()?;
                Save::new(bytes).ok()
            })
            .collect::<Vec<_>>();
        saves.sort_by_key(Save::file_name);
        saves
    }

    fn projection_region(primary: &[SaveDepotId]) -> Result<Region, Error> {
        let mut region = None;
        for id in primary {
            let code = GameCubeDiscId::try_from(*id).map_err(|_| {
                Error::Unsupported("a GameCube primary must be a GameCube product code".to_owned())
            })?;
            let candidate = Region::from_country_code(code.as_bytes()[3]);
            if region.is_some_and(|region| region != candidate) {
                return Err(Error::Unsupported(
                    "a GameCube projection cannot mix primary regions".to_owned(),
                ));
            }
            region = Some(candidate);
        }
        region.ok_or_else(|| {
            Error::Unsupported("a GameCube projection requires a primary game".to_owned())
        })
    }

    fn project(&self, primary: &[SaveDepotId]) -> Result<(Region, Vec<Save<'static>>), Error> {
        let region = Self::projection_region(primary)?;
        // A GameCube disc's number is separate from its six-byte product code. All
        // saves belonging to every disc of the primary title therefore share the
        // same depot ID and are required together.
        let required_ids = primary.iter().map(|id| id.0).collect::<HashSet<_>>();
        let mut candidates = self
            .saves()
            .into_iter()
            // A raw card has one text encoding, so optional fill must never pull
            // saves from a different region into either projection format.
            .filter(|save| save.region() == region)
            .collect::<Vec<_>>();
        let mut identities = HashSet::new();
        if candidates
            .iter()
            .any(|save| !identities.insert(save.identity()))
        {
            return Err(Error::Unsupported(
                "canonical GameCube storage contains duplicate save identities".to_owned(),
            ));
        }
        candidates
            .sort_by_key(|save| (!required_ids.contains(&save.depot_id().0), save.file_name()));

        let mut projected = Vec::new();
        let mut blocks = 0u32;
        let capacity_blocks = u32::from(raw::card::MC251_BLOCKS);
        let target_blocks = capacity_blocks * TARGET_CAPACITY_PERCENT / 100;
        for save in candidates {
            let is_required = required_ids.contains(&save.depot_id().0);
            let save_blocks = u32::from(save.block_count());
            if projected.len() == raw::card::DIRECTORY_ENTRIES
                || blocks + save_blocks > capacity_blocks
            {
                if is_required {
                    return Err(Error::Unsupported(
                        "the required GameCube saves do not fit on an MC-251".to_owned(),
                    ));
                }
                continue;
            }
            if !is_required && blocks + save_blocks > target_blocks {
                continue;
            }
            blocks += save_blocks;
            projected.push(save);
        }
        Ok((region, projected))
    }

    fn reconcile(
        &self,
        region: Region,
        projected: &HashMap<Vec<u8>, String>,
        saves: &[Save<'_>],
    ) -> Result<(), Error> {
        let mut decoded = HashMap::new();
        let mut paths = HashSet::new();
        for save in saves {
            if save.region() != region {
                return Err(Error::Unsupported(
                    "a GameCube projection cannot contain saves from another region".to_owned(),
                ));
            }
            let path = format!("{SAVE_ROOT}/{}", save.file_name());
            if decoded.insert(save.identity(), path.clone()).is_some() {
                return Err(Error::Unsupported(
                    "a GameCube projection contains duplicate save identities".to_owned(),
                ));
            }
            if !paths.insert(path) {
                return Err(Error::Unsupported(
                    "a GameCube projection contains colliding GCI filenames".to_owned(),
                ));
            }
        }

        for save in saves {
            let path = decoded
                .get(&save.identity())
                .expect("decoded saves have canonical paths");
            self.write_if_changed(path, save.as_bytes())?;
        }
        for (identity, path) in projected {
            if decoded.get(identity) != Some(path) {
                self.storage.remove(path)?;
            }
        }
        Ok(())
    }

    fn write_if_changed(&self, path: &str, bytes: &[u8]) -> Result<(), Error> {
        if self
            .storage
            .metadata(path)
            .is_ok_and(|metadata| metadata.len == bytes.len() as u64)
            && let Ok(mut existing) = self.storage.open_read(path)
        {
            let mut current = Vec::with_capacity(bytes.len());
            if existing.read_to_end(&mut current).is_ok() && current == bytes {
                return Ok(());
            }
        }
        let output = self.storage.create(path)?;
        output.write_all_at(0, bytes)?;
        output.set_len(bytes.len() as u64)?;
        output.sync()
    }

    fn describe_save(&self, save: &Save<'_>) -> Result<SaveDescriptor, Error> {
        let (title, description) = save
            .comments()
            .map(|(title, description)| (title.into_owned(), description.into_owned()))
            .unwrap_or_else(|| {
                (
                    String::from_utf8_lossy(save.console_name()).into_owned(),
                    String::from_utf8_lossy(save.game_code()).into_owned(),
                )
            });
        let path = format!("{SAVE_ROOT}/{}", save.file_name());
        let timestamp = self
            .storage
            .metadata(&path)
            .map(|metadata| metadata.mtime)
            .unwrap_or_else(|_| Timestamp::now());
        Ok(SaveDescriptor {
            id: save.depot_id(),
            title,
            description,
            timestamp,
            root_path: path,
        })
    }

    fn snapshot(&self, saves: &[SaveDepotId]) -> Vec<(Vec<u8>, Option<(u64, Timestamp)>)> {
        let selected = saves.iter().map(|id| id.0).collect::<HashSet<_>>();
        self.saves()
            .into_iter()
            .filter(|save| selected.is_empty() || selected.contains(&save.depot_id().0))
            .map(|save| {
                let path = format!("{SAVE_ROOT}/{}", save.file_name());
                let metadata = self
                    .storage
                    .metadata(&path)
                    .ok()
                    .map(|metadata| (metadata.len, metadata.mtime));
                (save.identity(), metadata)
            })
            .collect()
    }
}

impl From<gci::ParseError> for Error {
    fn from(error: gci::ParseError) -> Self {
        Self::MalformedMetadata {
            file: "GameCube GCI".to_owned(),
            reason: error.to_string(),
        }
    }
}

impl From<raw::card::ParseError> for Error {
    fn from(error: raw::card::ParseError) -> Self {
        Self::MalformedMetadata {
            file: "GameCube memory card".to_owned(),
            reason: error.to_string(),
        }
    }
}

impl fmt::Debug for GameCubeMemoryCard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GameCubeMemoryCard")
            .field("storage", &self.storage)
            .finish()
    }
}

impl MemoryCard for GameCubeMemoryCard {
    fn storage(&self) -> &dyn Storage {
        self.storage.as_ref()
    }

    fn reconcile(&self, primary: &[SaveDepotId]) -> Result<crate::Reconcile, Error> {
        let (region, projected) = self.project(primary)?;
        let projected_paths = projected
            .iter()
            .map(|save| (save.identity(), format!("{SAVE_ROOT}/{}", save.file_name())))
            .collect::<HashMap<_, _>>();
        let card = self.clone();
        let blocks = u32::from(raw::card::MC251_BLOCKS);
        Ok(Arc::new(move |bytes| {
            let image = raw::card::MemoryCard::new(bytes, blocks)?;
            let saves = image.saves()?;
            card.reconcile(region, &projected_paths, &saves)
        }))
    }

    fn view(&self, primary: &[SaveDepotId], format: &str) -> Result<MemoryCardView, Error> {
        let (region, projected) = self.project(primary)?;
        let projected_paths = projected
            .iter()
            .map(|save| (save.identity(), format!("{SAVE_ROOT}/{}", save.file_name())))
            .collect::<HashMap<_, _>>();
        let card = self.clone();
        let reconcile = move |saves: &[Save<'_>]| card.reconcile(region, &projected_paths, saves);
        let storage: Arc<dyn MountableStorage> = match format {
            "dolphin" => Arc::new(DolphinMemoryStorage::new(&projected, reconcile)?),
            "raw" => {
                let blocks = u32::from(raw::card::MC251_BLOCKS);
                let mut image = raw::card::MemoryCard::empty(blocks)?;
                image.initialize_header(self.id, region)?;
                for save in &projected {
                    image.append_save(Save::new(save.as_bytes())?)?;
                }
                Arc::new(MemoryCardStorage::<Self>::new(
                    self.clone(),
                    primary,
                    image.into_owned(),
                    "gamecube-memory-card",
                    "GameCube memory card",
                )?)
            }
            _ => {
                return Err(Error::Unsupported(format!(
                    "GameCube memory cards do not support the '{format}' format"
                )));
            }
        };
        MemoryCardView::new(storage, "")
    }
}

impl SaveGameSink for GameCubeMemoryCard {
    fn infer_save_depot(&self, package: &Package) -> SaveDepotId {
        let serial = package
            .serial()
            .expect("a GameCube package must expose a product code");
        GameCubeDiscId::new(serial.trim().as_bytes())
            .expect("a GameCube product code contains six ASCII letters or digits")
            .into()
    }

    fn enumerate_saves(&self) -> SaveDescriptors<'_> {
        let mut seen = HashSet::new();
        Box::new(
            self.saves()
                .into_iter()
                .filter(|save| seen.insert(save.depot_id().0))
                .filter_map(|save| self.describe_save(&save).ok())
                .collect::<Vec<_>>()
                .into_iter(),
        )
    }

    fn export_save(&self, id: &SaveDepotId) -> Result<SaveDepot, Error> {
        let saves = self
            .saves()
            .into_iter()
            .filter(|save| save.depot_id() == *id)
            .collect::<Vec<_>>();
        let representative = saves
            .first()
            .ok_or_else(|| Error::InvalidPath(format!("unknown save depot {:016x}", id.0)))?;
        let save_descriptor = self.describe_save(representative)?;
        let blobs = saves
            .into_iter()
            .map(|save| SaveBlob {
                target_path: PathBuf::from(format!("{SAVE_ROOT}/{}", save.file_name())),
                blake3: *blake3::hash(save.as_bytes()).as_bytes(),
                bytes: save.as_bytes().to_vec(),
            })
            .collect();
        Ok(SaveDepot {
            save_descriptor,
            blobs,
        })
    }

    fn install_save<'a>(
        &'a self,
        blobs: SaveBlobStream<'a>,
    ) -> BoxFuture<'a, Result<SaveDescriptor, Error>> {
        Box::pin(async move {
            let mut blobs = blobs;
            let mut installed = None;
            let mut representative = None;
            while let Some(blob) = blobs.next().await {
                if *blake3::hash(&blob.bytes).as_bytes() != blob.blake3 {
                    return Err(Error::IntegrityMatchFailed {
                        failed: blob.target_path.to_string_lossy().into_owned(),
                    });
                }
                let save = Save::new(blob.bytes)?;
                if installed.is_some_and(|id| id != save.depot_id()) {
                    return Err(Error::Unsupported(
                        "a GameCube save install may contain only one depot".to_owned(),
                    ));
                }
                let path = format!("{SAVE_ROOT}/{}", save.file_name());
                self.write_if_changed(&path, save.as_bytes())?;
                installed = Some(save.depot_id());
                representative.get_or_insert(save);
            }
            self.describe_save(&representative.ok_or(Error::InstallIncomplete)?)
        })
    }

    fn watch_saves(&self, saves: &[SaveDepotId]) -> Result<SaveWatcher, Error> {
        let activity = self
            .storage
            .watch()?
            .ok_or_else(|| Error::Unsupported("GameCube storage cannot watch saves".to_owned()))?;
        let card = self.clone();
        let selected = saves.to_vec();
        Ok(SaveWatcher::with_fingerprint(
            selected.clone(),
            activity,
            Duration::from_millis(100),
            move || card.snapshot(&selected),
        ))
    }
}
