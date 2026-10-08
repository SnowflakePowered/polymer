mod pcsx2;
mod psu;
mod raw;

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use encoding_rs::SHIFT_JIS;
use futures::StreamExt;
use jiff::Timestamp;
use parking_lot::Mutex;
use uuid::Uuid;

use self::pcsx2::Pcsx2MemoryStorage;
pub(crate) use self::psu::Save as IconSaveContainer;
use self::psu::Save;
use crate::impls::SonyProductCode;
use crate::impls::memorycard::RawMemoryCardImage;
use crate::savegame::{
    Package, SaveBlob, SaveBlobStream, SaveDepot, SaveDepotId, SaveDescriptor, SaveDescriptors,
    SaveGameSink, SaveWatcher, icons::IconSave,
};
use crate::savegroups::{PS2_SAVE_GROUPS, TARGET_CAPACITY_PERCENT};
use crate::storage::VolumeOverlay;
use crate::{
    BoxFuture, EntryKind, Error, MemoryCard, MemoryCardView, SaveRepo, Storage, VolumeStorage,
};

const SAVE_ROOT: &str = "card";
const CAPACITY_BLOCKS: u32 = 1024;
const PRODUCT_CODE_SIZE: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
struct SaveDataHandle {
    name: Vec<u8>,
}

impl SaveDataHandle {
    fn from_name(name: &[u8]) -> Option<Self> {
        (!name.is_empty()
            && name.len() <= 32
            && !name.contains(&0)
            && !matches!(name, b"." | b".."))
        .then(|| Self {
            name: name.to_vec(),
        })
    }

    fn display_name(&self) -> Cow<'_, str> {
        SHIFT_JIS.decode_without_bom_handling(&self.name).0
    }

    fn file_name(&self) -> String {
        let plain = self.name.is_ascii()
            && !matches!(self.name.first(), Some(b'.' | b'~'))
            && !matches!(self.name.last(), Some(b'.' | b' '))
            && self
                .name
                .iter()
                .all(|byte| *byte >= 0x20 && !b"<>:\"/\\|?*".contains(byte));
        let reserved = if plain {
            let uppercase = String::from_utf8_lossy(&self.name).to_ascii_uppercase();
            matches!(uppercase.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || uppercase
                    .strip_prefix("COM")
                    .or_else(|| uppercase.strip_prefix("LPT"))
                    .is_some_and(|suffix| {
                        suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9')
                    })
        } else {
            false
        };
        if plain && !reserved {
            std::str::from_utf8(&self.name)
                .expect("an ASCII name is UTF-8")
                .to_owned()
        } else {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let mut encoded = String::with_capacity(1 + self.name.len() * 2);
            encoded.push('~');
            for byte in &self.name {
                encoded.push(HEX[(byte >> 4) as usize] as char);
                encoded.push(HEX[(byte & 0x0f) as usize] as char);
            }
            encoded
        }
    }

    fn path(&self) -> String {
        format!("{SAVE_ROOT}/{}.{}", self.file_name(), psu::EXTENSION)
    }

    fn from_path(name: &str) -> Option<Self> {
        let stem = name.strip_suffix(".psu")?;
        let handle = if let Some(hex) = stem.strip_prefix('~') {
            if hex.is_empty() || hex.len() % 2 != 0 || !hex.is_ascii() {
                return None;
            }
            let bytes = hex.as_bytes();
            let mut name = Vec::with_capacity(bytes.len() / 2);
            for pair in bytes.chunks_exact(2) {
                let nibble = |byte| match byte {
                    b'0'..=b'9' => Some(byte - b'0'),
                    b'a'..=b'f' => Some(byte - b'a' + 10),
                    _ => None,
                };
                name.push((nibble(pair[0])? << 4) | nibble(pair[1])?);
            }
            Self::from_name(&name)?
        } else {
            if !stem.is_ascii() {
                return None;
            }
            Self::from_name(stem.as_bytes())?
        };
        (handle.file_name() == stem).then_some(handle)
    }

    fn depot_id(&self) -> SaveDepotId {
        let product_code = self.name.get(..PRODUCT_CODE_SIZE).unwrap_or(&self.name);
        SonyProductCode::new(product_code)
            .expect("a PlayStation 2 save has a product-code native name")
            .into()
    }

    fn depot_id_from_serial(serial: &str) -> SaveDepotId {
        let serial = serial.trim().to_ascii_uppercase();
        let key = match serial.get(..4) {
            Some("SLPS" | "SCPS" | "SIPS") => b"BI",
            Some("SLUS" | "SCUS") => b"BA",
            Some("SLES" | "SCES") => b"BE",
            _ => panic!("unsupported PlayStation 2 product serial `{serial}`"),
        };
        assert_eq!(
            serial.len(),
            PRODUCT_CODE_SIZE - key.len(),
            "PlayStation 2 product serial `{serial}` does not fit a memory-card product code"
        );
        let mut product_code = [0; PRODUCT_CODE_SIZE];
        product_code[..key.len()].copy_from_slice(key);
        product_code[key.len()..].copy_from_slice(serial.as_bytes());
        SonyProductCode::new(&product_code)
            .expect("a package serial forms a product code")
            .into()
    }
}

/// A canonical, filesystem-backed PlayStation 2 memory card.
///
/// The storage contains only console-visible saves under `card/`. Emulator-specific
/// files and raw filesystem structures are materialized by [`MemoryCard::view`].
#[derive(Clone)]
pub struct Playstation2MemoryCard {
    storage: Arc<crate::VersionedStorage>,
}

impl VolumeOverlay for Playstation2MemoryCard {
    fn inner(&self) -> &dyn VolumeStorage {
        self.storage.as_ref()
    }
}

#[async_trait::async_trait]
impl SaveRepo for Playstation2MemoryCard {
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
        Ok(self.read_save(&save)?.read_icon())
    }
}

impl Playstation2MemoryCard {
    pub const PLATFORM: &str = "SONY_PS2";

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
                "`{storage:?}` is a {} volume",
                identity.platform
            )));
        }
        storage.create_dir_all(SAVE_ROOT)?;
        Ok(Self { storage })
    }

    fn saves(&self) -> Vec<SaveDataHandle> {
        let Ok(entries) = self.storage.read_dir(SAVE_ROOT, false) else {
            return Vec::new();
        };
        let mut saves = entries
            .filter(|entry| entry.metadata.kind == EntryKind::File)
            .filter_map(|entry| SaveDataHandle::from_path(&entry.name))
            .collect::<Vec<_>>();
        saves.sort_by(|left, right| left.name.cmp(&right.name));
        saves
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>, Error> {
        let mut bytes = Vec::new();
        self.storage
            .open_read(path)?
            .read_to_end(&mut bytes)
            .map_err(|source| Error::Io {
                path: path.to_owned(),
                source,
            })?;
        Ok(bytes)
    }

    fn saves_in_depot(&self, id: SaveDepotId) -> Vec<SaveDataHandle> {
        self.saves()
            .into_iter()
            .filter(|save| save.depot_id() == id)
            .collect()
    }

    fn describe_save(&self, save: &SaveDataHandle) -> Result<SaveDescriptor, Error> {
        let timestamp = self
            .storage
            .metadata(&save.path())
            .map(|metadata| metadata.mtime)
            .unwrap_or_else(|_| Timestamp::now());
        let title = self.read_save(save)?.title();
        Ok(SaveDescriptor {
            id: save.depot_id(),
            title: title.unwrap_or_else(|| save.display_name().into_owned()),
            description: save.display_name().into_owned(),
            timestamp,
            root_path: save.path(),
        })
    }

    fn read_save(&self, handle: &SaveDataHandle) -> Result<Save<'static>, Error> {
        let path = handle.path();
        let bytes = self.read_file(&path)?;
        let save = Save::new(bytes).map_err(|error| Error::MalformedMetadata {
            file: path.clone(),
            reason: error.to_string(),
        })?;
        if save.name_bytes() != handle.name {
            return Err(Error::MalformedMetadata {
                file: path,
                reason: format!(
                    "PSU contains save `{}` instead of `{}`",
                    save.name(),
                    handle.display_name()
                ),
            });
        }
        Ok(save)
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

    fn project_saves(
        &self,
        primary: &[SaveDepotId],
        capacity_blocks: u32,
    ) -> Result<Vec<Save<'static>>, Error> {
        let primary_ids = primary.iter().map(|id| id.0).collect::<HashSet<_>>();
        let grouped_ids = PS2_SAVE_GROUPS.members(primary);
        let target_blocks = capacity_blocks * TARGET_CAPACITY_PERCENT / 100;
        let mut projected = Vec::<Save<'static>>::new();
        for handle in self.projection_order(&primary_ids, &grouped_ids) {
            let save = self.read_save(&handle)?;
            let id = handle.depot_id().0;
            let preferred = primary_ids.contains(&id) || grouped_ids.contains(&id);
            projected.push(save);
            if !raw::card::MemoryCard::fits(capacity_blocks, &projected)? {
                projected.pop();
                if primary_ids.contains(&id) {
                    return Err(Error::Unsupported(format!(
                        "primary PlayStation 2 save `{}` does not fit the requested card capacity",
                        handle.display_name()
                    )));
                }
            } else if !preferred && !raw::card::MemoryCard::fits(target_blocks, &projected)? {
                projected.pop();
            }
        }
        Ok(projected)
    }

    fn reconcile(&self, projected: &[Vec<u8>], saves: &[Save<'_>]) -> Result<(), Error> {
        let decoded = saves
            .iter()
            .map(|save| save.name_bytes().to_vec())
            .collect::<HashSet<_>>();
        for save in saves {
            self.write_save(save)?;
        }
        for name in projected {
            if !decoded.contains(name) {
                let handle = SaveDataHandle::from_name(name).ok_or_else(|| {
                    Error::InvalidPath(String::from_utf8_lossy(name).into_owned())
                })?;
                self.storage.remove(&handle.path())?;
            }
        }
        Ok(())
    }

    fn write_save(&self, save: &Save<'_>) -> Result<(), Error> {
        let handle = SaveDataHandle::from_name(save.name_bytes())
            .ok_or_else(|| Error::InvalidPath(save.name().into_owned()))?;
        let path = handle.path();
        self.storage.install_bytes(save.as_bytes(), &path)?;
        Ok(())
    }

    fn snapshot(&self, saves: &[SaveDepotId]) -> Vec<(String, u64, Timestamp)> {
        let selected = if saves.is_empty() {
            None
        } else {
            Some(saves.iter().map(|id| id.0).collect::<HashSet<_>>())
        };
        let mut snapshot = Vec::new();
        for save in self.saves() {
            if selected
                .as_ref()
                .is_some_and(|ids| !ids.contains(&save.depot_id().0))
            {
                continue;
            }
            if let Ok(metadata) = self.storage.metadata(&save.path()) {
                snapshot.push((save.path(), metadata.len, metadata.mtime));
            }
        }
        snapshot.sort_by(|left, right| left.0.cmp(&right.0));
        snapshot
    }

    fn validate_capacity(capacity_blocks: u32) -> Result<(), Error> {
        match capacity_blocks {
            1024 | 2048 | 4096 | 8192 => Ok(()),
            _ => Err(Error::Unsupported(format!(
                "PlayStation 2 cards support 1024, 2048, 4096, or 8192 blocks, not {capacity_blocks}"
            ))),
        }
    }
}

impl fmt::Debug for Playstation2MemoryCard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Playstation2MemoryCard")
            .field("storage", &self.storage)
            .finish()
    }
}

impl MemoryCard for Playstation2MemoryCard {
    const RAW_RECONCILE_NAME: &'static str = "ps2-memory-card";

    fn storage(&self) -> &dyn Storage {
        self.storage.as_ref()
    }

    fn reconcile(&self, primary: &[SaveDepotId]) -> Result<crate::Reconcile, Error> {
        Self::validate_capacity(CAPACITY_BLOCKS)?;
        let projected = self
            .project_saves(primary, CAPACITY_BLOCKS)?
            .iter()
            .map(|save| save.name_bytes().to_vec())
            .collect::<Vec<_>>();
        let card = self.clone();
        let owned = Arc::new(Mutex::new(projected));
        Ok(Arc::new(move |bytes| {
            let image = raw::card::MemoryCard::new(bytes, CAPACITY_BLOCKS)?;
            let Ok(mut saves) = image.saves() else {
                return Ok(());
            };
            let had_incomplete = saves.iter().any(|save| save.files().len() == 0);
            saves.retain(|save| save.files().len() != 0);
            if had_incomplete && saves.is_empty() {
                return Ok(());
            }
            let previous = owned.lock().clone();
            card.reconcile(&previous, &saves)?;
            *owned.lock() = saves
                .iter()
                .map(|save| save.name_bytes().to_vec())
                .collect();
            Ok(())
        }))
    }

    fn view(&self, primary: &[SaveDepotId], format: &str) -> Result<MemoryCardView, Error> {
        Self::validate_capacity(CAPACITY_BLOCKS)?;
        match format {
            "raw" => self.raw_view(primary),
            "pcsx2" => MemoryCardView::new(
                Arc::new(crate::ReconcileStorage::new(
                    Pcsx2MemoryStorage::new(
                        self.clone(),
                        CAPACITY_BLOCKS,
                        self.project_saves(primary, CAPACITY_BLOCKS)?,
                    )?,
                    "polymer-pcsx2-writeback",
                )?),
                "",
            ),
            _ => Err(Error::Unsupported(format!(
                "PlayStation 2 memory cards do not support the '{format}' format"
            ))),
        }
    }

    fn raw_image(&self, primary: &[SaveDepotId]) -> Result<Vec<u8>, Error> {
        let saves = self.project_saves(primary, CAPACITY_BLOCKS)?;
        Ok(raw::card::MemoryCard::format(
            CAPACITY_BLOCKS,
            &saves,
            true,
        )?)
    }
}

impl SaveGameSink for Playstation2MemoryCard {
    fn infer_save_depot(&self, package: &Package) -> SaveDepotId {
        let serial = package
            .serial()
            .expect("a PlayStation 2 package must expose a product serial");
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
        let blobs = saves
            .into_iter()
            .map(|save| {
                let path = save.path();
                let bytes = self.read_file(&path)?;
                Ok(SaveBlob {
                    target_path: PathBuf::from(path),
                    blake3: *blake3::hash(&bytes).as_bytes(),
                    bytes,
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
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
                let relative = Path::new(&target)
                    .strip_prefix(SAVE_ROOT)
                    .ok()
                    .filter(|path| path.components().count() == 1)
                    .and_then(|path| path.file_name())
                    .and_then(|name| name.to_str())
                    .and_then(SaveDataHandle::from_path)
                    .ok_or_else(|| Error::InvalidPath(target.clone()))?;
                if installed.is_some_and(|installed| installed != relative.depot_id()) {
                    return Err(Error::Unsupported(
                        "a PlayStation 2 save install may contain only one depot".to_owned(),
                    ));
                }
                let parsed =
                    Save::new(blob.bytes.as_slice()).map_err(|error| Error::MalformedMetadata {
                        file: target.clone(),
                        reason: error.to_string(),
                    })?;
                if parsed.name_bytes() != relative.name {
                    return Err(Error::MalformedMetadata {
                        file: target,
                        reason: format!("PSU contains save `{}`", parsed.name()),
                    });
                }
                let written = self
                    .storage
                    .install_file(Box::new(Cursor::new(blob.bytes)), &target)
                    .await?;
                if written.blake3 != blob.blake3 {
                    return Err(Error::IntegrityMatchFailed { failed: target });
                }
                installed = Some(relative.depot_id());
                representative.get_or_insert(relative);
            }
            let save = representative.ok_or(Error::InstallIncomplete)?;
            self.describe_save(&save)
        })
    }

    fn watch_saves(&self, saves: &[SaveDepotId]) -> Result<SaveWatcher, Error> {
        let activity = self.storage.watch()?.ok_or_else(|| {
            Error::Unsupported("PlayStation 2 storage cannot watch saves".to_owned())
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
