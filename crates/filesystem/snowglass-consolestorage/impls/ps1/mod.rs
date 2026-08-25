mod raw;

use std::collections::HashSet;
use std::fmt;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use jiff::Timestamp;
use uuid::Uuid;

pub(crate) use self::raw::Save as IconSaveContainer;
use self::raw::{BLOCK_SIZE, NATIVE_ID_SIZE};
use crate::impls::{
    SonyProductCode,
    memorycard::{MemoryCardStorage, RawMemoryCardImage},
};
use crate::savegame::{
    Package, SaveBlob, SaveBlobStream, SaveDepot, SaveDepotId, SaveDescriptor, SaveDescriptors,
    SaveGameSink, SaveWatcher, icons::IconSave,
};
use crate::savegroups::{PS1_SAVE_GROUPS, TARGET_CAPACITY_PERCENT};
use crate::storage::VolumeOverlay;
use crate::{
    BoxFuture, EntryKind, Error, MemoryCard, MemoryCardView, MountableStorage, SaveRepo, Storage,
    VolumeStorage,
};

const SAVE_HEADER_SIZE: usize = 12;
const SAVE_ROOT: &str = "card";

#[derive(Clone, Debug, PartialEq, Eq)]
struct SaveDataHandle {
    native_id: [u8; NATIVE_ID_SIZE],
}

impl SaveDataHandle {
    fn from_native_id(native_id: [u8; NATIVE_ID_SIZE]) -> Self {
        Self { native_id }
    }

    fn from_native_name(name: &str) -> Option<Self> {
        let bytes = name.as_bytes();
        if bytes.is_empty()
            || bytes.len() > NATIVE_ID_SIZE
            || name.starts_with('.')
            || name.contains(['/', '\\'])
        {
            return None;
        }
        let mut native_id = [0; NATIVE_ID_SIZE];
        native_id[..bytes.len()].copy_from_slice(bytes);
        Some(Self::from_native_id(native_id))
    }

    fn from_data_path(path: &str) -> Option<Self> {
        let mut components = path.split('/');
        if components.next()? != SAVE_ROOT {
            return None;
        }
        let save = Self::from_native_name(components.next()?)?;
        components.next().is_none().then_some(save)
    }

    fn native_name(&self) -> String {
        String::from_utf8_lossy(&self.native_id)
            .trim_end_matches('\0')
            .to_owned()
    }

    fn data_path(&self) -> String {
        format!("{SAVE_ROOT}/{}", self.native_name())
    }

    fn depot_id(&self) -> SaveDepotId {
        SonyProductCode::new(&self.native_id[..SAVE_HEADER_SIZE])
            .expect("a PlayStation save has a product-code native ID")
            .into()
    }

    fn depot_id_from_serial(serial: &str) -> SaveDepotId {
        let serial = serial.trim().to_ascii_uppercase();
        let key = match serial.get(..4) {
            Some("SLPS" | "SCPS" | "SIPS") => b"BI",
            Some("SLUS" | "SCUS") => b"BA",
            Some("SLES" | "SCES") => b"BE",
            _ => panic!("unsupported PlayStation product serial `{serial}`"),
        };
        assert_eq!(
            serial.len(),
            SAVE_HEADER_SIZE - key.len(),
            "PlayStation product serial `{serial}` does not fit a memory-card header"
        );
        let mut header = [0; SAVE_HEADER_SIZE];
        header[..key.len()].copy_from_slice(key);
        header[key.len()..].copy_from_slice(serial.as_bytes());
        SonyProductCode::new(&header)
            .expect("a package serial forms a product code")
            .into()
    }
}

impl From<raw::ParseError> for Error {
    fn from(error: raw::ParseError) -> Self {
        Self::MalformedMetadata {
            file: "PlayStation memory card".to_owned(),
            reason: error.to_string(),
        }
    }
}

#[derive(Clone)]
pub struct PlaystationMemoryCard {
    storage: Arc<crate::VersionedStorage>,
}

impl VolumeOverlay for PlaystationMemoryCard {
    fn inner(&self) -> &dyn VolumeStorage {
        self.storage.as_ref()
    }
}

#[async_trait::async_trait]
impl SaveRepo for PlaystationMemoryCard {
    fn versioning(&self) -> &crate::SaveVersioning {
        self.storage.versioning()
    }

    fn read_save_icon(
        &self,
        id: SaveDepotId,
    ) -> Result<Option<crate::savegame::icons::SaveIcon>, Error> {
        let Some(save) = self.saves_in_depot(id).into_iter().next() else {
            return Ok(None);
        };
        let payload = self.read_payload(&save)?;
        Ok(raw::Save::new(&save.native_id, &payload)?.read_icon())
    }
}

impl PlaystationMemoryCard {
    pub const PLATFORM: &str = "SONY_PSX";

    pub async fn new_empty(
        storage: crate::VersionedStorage,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Self, Error> {
        let storage = Arc::new(storage);
        storage.create_dir_all(SAVE_ROOT)?;
        storage.brand(id, Self::PLATFORM, label.as_deref()).await?;
        Ok(Self { storage })
    }

    pub async fn open(storage: crate::VersionedStorage) -> Result<Self, Error> {
        let storage = Arc::new(storage);
        let identity = storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;
        if identity.platform != Self::PLATFORM {
            return Err(Error::Unsupported(format!(
                "`{:?}` is a {} volume",
                storage, identity.platform
            )));
        }
        storage.create_dir_all(SAVE_ROOT)?;
        Ok(Self { storage })
    }

    pub async fn metadata(&self) -> Result<crate::VolumeInfo, Error> {
        self.storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)
    }

    fn saves(&self) -> Vec<SaveDataHandle> {
        let Ok(entries) = self.storage.read_dir(SAVE_ROOT, false) else {
            return Vec::new();
        };
        let mut saves = entries
            .filter(|entry| entry.metadata.kind == EntryKind::File)
            .filter_map(|entry| SaveDataHandle::from_native_name(&entry.name))
            .collect::<Vec<_>>();
        saves.sort_by(|left, right| left.native_id.cmp(&right.native_id));
        saves
    }

    fn saves_in_depot(&self, id: SaveDepotId) -> Vec<SaveDataHandle> {
        self.saves()
            .into_iter()
            .filter(|save| save.depot_id() == id)
            .collect()
    }

    fn read_payload(&self, save: &SaveDataHandle) -> Result<Vec<u8>, Error> {
        let path = save.data_path();
        let mut payload = Vec::new();
        self.storage
            .open_read(&path)?
            .read_to_end(&mut payload)
            .map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
        Ok(payload)
    }

    fn describe_save(&self, save: &SaveDataHandle) -> Result<SaveDescriptor, Error> {
        let payload = self.read_payload(save)?;
        let parsed = raw::Save::new(&save.native_id, &payload)?;
        let header = parsed.describe()?;
        let title = header.title.into_owned();
        let title = if title.is_empty() {
            save.native_name()
        } else {
            title
        };
        let timestamp = self
            .storage
            .metadata(&save.data_path())
            .map(|metadata| metadata.mtime)
            .unwrap_or_else(|_| Timestamp::now());
        Ok(SaveDescriptor {
            id: save.depot_id(),
            title,
            description: save.native_name(),
            timestamp,
            root_path: save.data_path(),
        })
    }

    fn projection_order(
        &self,
        primary: &HashSet<u64>,
        grouped: &HashSet<u64>,
    ) -> Vec<SaveDataHandle> {
        let mut saves = self.saves();
        saves.sort_by_key(|save| {
            let id = save.depot_id().0;
            if primary.contains(&id) {
                0
            } else if grouped.contains(&id) {
                1
            } else {
                2
            }
        });
        saves
    }

    fn project(&self, primary: &[SaveDepotId]) -> Result<(Vec<u8>, Vec<SaveDataHandle>), Error> {
        let capacity_blocks = <raw::MemoryCard<'_> as RawMemoryCardImage>::MAX_BLOCKS;
        let target_blocks = capacity_blocks * TARGET_CAPACITY_PERCENT / 100;
        let primary_ids: HashSet<u64> = primary.iter().map(|id| id.0).collect();
        let grouped_ids = PS1_SAVE_GROUPS.members(primary);
        let mut card = raw::MemoryCard::empty(capacity_blocks)?;
        let mut projected = Vec::new();
        for save in self.projection_order(&primary_ids, &grouped_ids) {
            let payload = self.read_payload(&save)?;
            let blocks = payload.len().max(1).div_ceil(BLOCK_SIZE);
            let id = save.depot_id().0;
            let preferred = primary_ids.contains(&id) || grouped_ids.contains(&id);
            if card.next_block() + blocks > capacity_blocks as usize {
                if primary_ids.contains(&save.depot_id().0) {
                    return Err(Error::Unsupported(format!(
                        "primary saves require more than {capacity_blocks} PlayStation blocks"
                    )));
                }
                continue;
            }
            if !preferred && card.next_block() + blocks > target_blocks as usize {
                continue;
            }
            let raw_save = raw::Save::new(&save.native_id, &payload)?;
            card.append_save(raw_save)?;
            projected.push(save);
        }
        Ok((card.into_owned(), projected))
    }

    fn reconcile(&self, projected: &[SaveDataHandle], image: &[u8]) -> Result<(), Error> {
        let blocks = <raw::MemoryCard<'_> as RawMemoryCardImage>::MAX_BLOCKS;
        let card = raw::MemoryCard::new(image, blocks)?;
        let saves = card.saves()?;
        let decoded_names: HashSet<[u8; NATIVE_ID_SIZE]> =
            saves.iter().map(|save| *save.native_id()).collect();

        for raw_save in &saves {
            self.write_if_changed(
                &SaveDataHandle::from_native_id(*raw_save.native_id()),
                raw_save,
            )?;
        }
        for save in projected {
            if decoded_names.contains(&save.native_id) {
                continue;
            }
            self.storage.remove(&save.data_path())?;
        }
        Ok(())
    }

    fn write_if_changed(
        &self,
        save: &SaveDataHandle,
        raw_save: &raw::Save<'_>,
    ) -> Result<(), Error> {
        let path = save.data_path();
        if self
            .storage
            .metadata(&path)
            .is_ok_and(|metadata| metadata.len == raw_save.byte_len() as u64)
            && let Ok(mut existing) = self.storage.open_read(&path)
        {
            let mut block_buffer = [0; BLOCK_SIZE];
            let unchanged = raw_save.blocks().all(|block| {
                existing.read_exact(&mut block_buffer).is_ok() && block_buffer == block
            });
            if unchanged {
                return Ok(());
            }
        }

        let output = self.storage.create(&path)?;
        let mut offset = 0;
        for block in raw_save.blocks() {
            output.write_all_at(offset, block)?;
            offset += block.len() as u64;
        }
        output.sync()
    }

    fn snapshot(&self, saves: &[SaveDepotId]) -> Vec<(u64, Option<(u64, Timestamp)>)> {
        let selected = if saves.is_empty() {
            self.saves()
        } else {
            let ids = saves.iter().map(|id| id.0).collect::<HashSet<_>>();
            self.saves()
                .into_iter()
                .filter(|save| ids.contains(&save.depot_id().0))
                .collect()
        };
        selected
            .into_iter()
            .map(|save| {
                let metadata = self
                    .storage
                    .metadata(&save.data_path())
                    .ok()
                    .map(|metadata| (metadata.len, metadata.mtime));
                (save.depot_id().0, metadata)
            })
            .collect()
    }
}

impl fmt::Debug for PlaystationMemoryCard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaystationMemoryCard")
            .field("storage", &self.storage)
            .finish()
    }
}

impl MemoryCard for PlaystationMemoryCard {
    fn storage(&self) -> &dyn Storage {
        self.storage.as_ref()
    }

    fn reconcile(&self, primary: &[SaveDepotId]) -> Result<crate::Reconcile, Error> {
        let (_, projected) = self.project(primary)?;
        let card = self.clone();
        Ok(Arc::new(move |bytes| card.reconcile(&projected, bytes)))
    }

    fn view(&self, primary: &[SaveDepotId], format: &str) -> Result<MemoryCardView, Error> {
        if format != "raw" {
            return Err(Error::Unsupported(format!(
                "PlayStation memory cards do not support the '{format}' format"
            )));
        }
        let (image, _) = self.project(primary)?;
        let storage: Arc<dyn MountableStorage> = Arc::new(MemoryCardStorage::<Self>::new(
            self.clone(),
            primary,
            image,
            "ps1-memory-card",
            "PlayStation memory card",
        )?);
        MemoryCardView::new(storage, "")
    }
}

impl SaveGameSink for PlaystationMemoryCard {
    fn infer_save_depot(&self, package: &Package) -> SaveDepotId {
        let serial = package
            .serial()
            .expect("a PlayStation package must expose a product serial");
        SaveDataHandle::depot_id_from_serial(serial)
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
        let saves = self.saves_in_depot(*id);
        let save = saves
            .first()
            .ok_or_else(|| Error::InvalidPath(format!("unknown save depot {:016x}", id.0)))?;
        let save_descriptor = self.describe_save(save)?;
        let mut blobs = Vec::with_capacity(saves.len());
        for save in saves {
            let bytes = self.read_payload(&save)?;
            blobs.push(SaveBlob {
                target_path: PathBuf::from(save.data_path()),
                blake3: *blake3::hash(&bytes).as_bytes(),
                bytes,
            });
        }
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
            let mut installed = None::<SaveDepotId>;
            let mut representative = None;
            while let Some(blob) = blobs.next().await {
                let target = blob.target_path.to_string_lossy().replace('\\', "/");
                let save = SaveDataHandle::from_data_path(&target)
                    .ok_or_else(|| Error::InvalidPath(target.clone()))?;
                if installed.is_some_and(|installed| installed != save.depot_id()) {
                    return Err(Error::Unsupported(
                        "a PlayStation save install may contain only one depot".to_owned(),
                    ));
                }
                let written = self
                    .storage
                    .install_file(Box::new(Cursor::new(blob.bytes)), &save.data_path())
                    .await?;
                if written.blake3 != blob.blake3 {
                    return Err(Error::IntegrityMatchFailed { failed: target });
                }
                installed = Some(save.depot_id());
                representative.get_or_insert(save);
            }
            let save = representative.ok_or(Error::InstallIncomplete)?;
            self.describe_save(&save)
        })
    }

    fn watch_saves(&self, saves: &[SaveDepotId]) -> Result<SaveWatcher, Error> {
        let activity = self.storage.watch()?.ok_or_else(|| {
            Error::Unsupported("PlayStation storage cannot watch saves".to_owned())
        })?;
        let card = self.clone();
        let selected = saves.to_vec();
        Ok(SaveWatcher::with_fingerprint(
            selected.clone(),
            activity,
            Duration::from_millis(50),
            move || card.snapshot(&selected),
        ))
    }
}
