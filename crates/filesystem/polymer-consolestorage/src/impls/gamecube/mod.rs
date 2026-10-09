mod dolphin;
pub mod gci;
mod raw;

use std::borrow::Cow;
use std::collections::HashSet;
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
use crate::impls::memorycard::{RawMemoryCardImage, WriteBack, WriteBackEntry};
use crate::savegame::{
    Package, SaveBlob, SaveBlobStream, SaveDepot, SaveDepotId, SaveDescriptor, SaveDescriptors,
    SaveGameSink, SaveWatcher, icons::IconSave,
};
use crate::savegroups::TARGET_CAPACITY_PERCENT;
use crate::storage::VolumeOverlay;
use crate::{
    BoxFuture, EntryKind, Error, MemoryCard, MemoryCardView, SaveRepo, SaveVersioning, Storage,
    VersionedStorage, VolumeStorage,
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

    fn blocks(&self, id: SaveDepotId) -> Result<Option<u64>, Error> {
        Ok(Some(
            self.saves()
                .iter()
                .filter(|save| save.depot_id() == id)
                .map(|save| u64::from(save.block_count()))
                .sum(),
        ))
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

    fn write_back(&self, projected: &[Save<'_>]) -> WriteBack<Vec<u8>> {
        WriteBack::new(
            self.storage.clone(),
            projected
                .iter()
                .map(|save| (save.identity(), vec![Self::save_path(save)])),
        )
    }

    fn save_path(save: &Save<'_>) -> String {
        format!("{SAVE_ROOT}/{}", save.file_name())
    }

    fn reconcile(
        write_back: &WriteBack<Vec<u8>>,
        region: Region,
        saves: &[(Vec<u8>, Option<Save<'_>>)],
    ) -> Result<(), Error> {
        let entries = saves
            .iter()
            .map(|(identity, save)| WriteBackEntry {
                identity: identity.clone(),
                paths: save.iter().map(Self::save_path).collect(),
                contents: save
                    .as_ref()
                    .filter(|save| save.region() == region)
                    .map(|save| vec![Cow::Borrowed(save.as_bytes())]),
            })
            .collect::<Vec<_>>();
        write_back.pass(&entries)
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
        let path = Self::save_path(save);
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
                let path = Self::save_path(&save);
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
    const RAW_RECONCILE_NAME: &'static str = "gamecube-memory-card";

    fn storage(&self) -> &dyn Storage {
        self.storage.as_ref()
    }

    fn reconcile(&self, primary: &[SaveDepotId]) -> Result<crate::Reconcile, Error> {
        let (region, projected) = self.project(primary)?;
        let write_back = self.write_back(&projected);
        let blocks = u32::from(raw::card::MC251_BLOCKS);
        Ok(Arc::new(move |bytes| {
            let Ok(image) = raw::card::MemoryCard::new(bytes, blocks) else {
                return Ok(());
            };
            let Ok(saves) = image.lenient_saves() else {
                return Ok(());
            };
            Self::reconcile(&write_back, region, &saves)
        }))
    }

    fn view(&self, primary: &[SaveDepotId], format: &str) -> Result<MemoryCardView, Error> {
        match format {
            "dolphin" => {
                let (region, projected) = self.project(primary)?;
                MemoryCardView::new(
                    Arc::new(crate::ReconcileStorage::new(
                        DolphinMemoryStorage::new(self.clone(), region, &projected)?,
                        "polymer-gci-writeback",
                    )?),
                    "",
                )
            }
            "raw" => self.raw_view(primary),
            _ => Err(Error::Unsupported(format!(
                "GameCube memory cards do not support the '{format}' format"
            ))),
        }
    }

    fn raw_image(&self, primary: &[SaveDepotId]) -> Result<Vec<u8>, Error> {
        let (region, projected) = self.project(primary)?;
        let mut image = raw::card::MemoryCard::empty(u32::from(raw::card::MC251_BLOCKS))?;
        image.initialize_header(self.id, region)?;
        let saves = projected
            .iter()
            .map(|save| Save::new(save.as_bytes()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(image.with_saves(saves)?)
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
                target_path: PathBuf::from(Self::save_path(&save)),
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
                let path = Self::save_path(&save);
                self.storage.write_if_changed(&path, save.as_bytes())?;
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
