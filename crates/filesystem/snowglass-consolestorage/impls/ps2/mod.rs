mod pcsx2;
mod psu;
mod raw;

use std::collections::HashSet;
use std::fmt;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use atomic_write_file::AtomicWriteFile;
use futures::{Stream, StreamExt};
use jiff::Timestamp;
use parking_lot::Mutex;
use uuid::Uuid;

use self::pcsx2::Pcsx2MemoryStorage;
use self::psu::Save;
use self::raw::RawMemoryStorage;
#[cfg(test)]
use crate::impls::RawMemoryCardImage;
use crate::savegame::{
    Package, SaveBlob, SaveDepot, SaveDepotId, SaveDescriptor, SaveGameSink, SaveWatcher,
};
use crate::{
    BoxFuture, ChangeSignal, DirectoryStorage, EntryKind, Error, InstallResult, MemoryCard,
    MemoryCardView, Metadata, MountableStorage, ReadDir, Storage, StorageFile, StorageRead,
    VolumeStorage,
};

const SAVE_ROOT: &str = "card";
const CAPACITY_BLOCKS: u32 = 1024;
const PRODUCT_CODE_SIZE: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
struct SaveDataHandle {
    name: String,
}

impl SaveDataHandle {
    fn from_name(name: &str) -> Option<Self> {
        (!name.is_empty()
            && !name.starts_with('.')
            && !name.starts_with("_pcsx2_")
            && !name.contains(['/', '\\']))
        .then(|| Self {
            name: name.to_owned(),
        })
    }

    fn path(&self) -> String {
        format!("{SAVE_ROOT}/{}.{}", self.name, psu::EXTENSION)
    }

    fn from_path(name: &str) -> Option<Self> {
        Self::from_name(name.strip_suffix(".psu")?)
    }

    fn depot_id(&self) -> SaveDepotId {
        let product_code = self
            .name
            .as_bytes()
            .get(..PRODUCT_CODE_SIZE)
            .unwrap_or(self.name.as_bytes());
        let digest = blake3::hash(product_code);
        SaveDepotId(u64::from_le_bytes(
            digest.as_bytes()[..8].try_into().expect("eight bytes"),
        ))
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
        let digest = blake3::hash(&product_code);
        SaveDepotId(u64::from_le_bytes(
            digest.as_bytes()[..8].try_into().expect("eight bytes"),
        ))
    }
}

/// A canonical, filesystem-backed PlayStation 2 memory card.
///
/// The storage contains only console-visible saves under `card/`. Emulator-specific
/// files and raw filesystem structures are materialized by [`MemoryCard::view`].
#[derive(Clone)]
pub struct Playstation2MemoryCard {
    storage: Arc<DirectoryStorage>,
}

impl Playstation2MemoryCard {
    pub const PLATFORM: &str = "SONY_PS2";

    pub async fn new_empty(
        storage: DirectoryStorage,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Self, Error> {
        let storage = Arc::new(storage);
        storage.create_dir_all(SAVE_ROOT)?;
        storage.brand(id, Self::PLATFORM, label.as_deref()).await?;
        Ok(Self { storage })
    }

    pub async fn open(storage: DirectoryStorage) -> Result<Self, Error> {
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

    pub async fn metadata(&self) -> Result<crate::NandMetadata, Error> {
        self.storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)
    }

    pub async fn game_saved(&self) -> Result<(), Error> {
        self.watch_saves(&[])?.changed().await;
        Ok(())
    }

    fn saves(&self) -> Vec<SaveDataHandle> {
        let Ok(entries) = self.storage.read_dir(SAVE_ROOT, false) else {
            return Vec::new();
        };
        let mut saves = entries
            .filter(|entry| entry.kind == EntryKind::File)
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
        Ok(SaveDescriptor {
            id: save.depot_id(),
            banner: None,
            title: save.name.clone(),
            description: save.name.clone(),
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
        if save.name().as_ref() != handle.name {
            return Err(Error::MalformedMetadata {
                file: path,
                reason: format!(
                    "PSU contains save `{}` instead of `{}`",
                    save.name(),
                    handle.name
                ),
            });
        }
        Ok(save)
    }

    fn projection_order(&self, primary: &[SaveDepotId]) -> Vec<SaveDataHandle> {
        let primary = primary.iter().map(|id| id.0).collect::<HashSet<_>>();
        let mut saves = self.saves();
        saves.sort_by_key(|save| !primary.contains(&save.depot_id().0));
        saves
    }

    fn project_saves(
        &self,
        primary: &[SaveDepotId],
        capacity_blocks: u32,
    ) -> Result<Vec<Save<'static>>, Error> {
        let primary_ids = primary.iter().map(|id| id.0).collect::<HashSet<_>>();
        let mut projected = Vec::<Save<'static>>::new();
        for handle in self.projection_order(primary) {
            let save = self.read_save(&handle)?;
            projected.push(save);
            if !raw::card::MemoryCard::fits(capacity_blocks, &projected)? {
                projected.pop();
                if primary_ids.contains(&handle.depot_id().0) {
                    return Err(Error::Unsupported(format!(
                        "primary PlayStation 2 save `{}` does not fit the requested card capacity",
                        handle.name
                    )));
                }
            }
        }
        Ok(projected)
    }

    fn reconcile(&self, projected: &[String], saves: &[Save<'_>]) -> Result<(), Error> {
        let decoded = saves
            .iter()
            .map(|save| save.name().into_owned())
            .collect::<HashSet<_>>();
        for save in saves {
            self.write_save(save)?;
        }
        for name in projected {
            if !decoded.contains(name) {
                let handle = SaveDataHandle::from_name(name)
                    .ok_or_else(|| Error::InvalidPath(name.clone()))?;
                self.storage.remove(&handle.path())?;
            }
        }
        Ok(())
    }

    fn write_save(&self, save: &Save<'_>) -> Result<(), Error> {
        let name = save.name();
        let handle = SaveDataHandle::from_name(&name)
            .ok_or_else(|| Error::InvalidPath(name.into_owned()))?;
        if save
            .files()
            .any(|file| SaveDataHandle::from_name(&file.name()).is_none())
        {
            return Err(Error::InvalidPath("invalid PSU file name".to_owned()));
        }
        let path = self.storage.root().join(handle.path());
        let mut output = AtomicWriteFile::open(&path).map_err(|source| Error::io(&path, source))?;
        output
            .write_all(save.as_bytes())
            .map_err(|source| Error::io(&path, source))?;
        output.flush().map_err(|source| Error::io(&path, source))?;
        output.commit().map_err(|source| Error::io(&path, source))
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

impl Storage for Playstation2MemoryCard {
    fn exists(&self, path: &str) -> bool {
        self.storage.exists(path)
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        self.storage.metadata(path)
    }

    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        self.storage.read_dir(path, recursive)
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        self.storage.create_dir_all(path)
    }

    fn create(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        self.storage.create(path)
    }

    fn open(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        self.storage.open(path)
    }

    fn open_read(&self, path: &str) -> Result<Box<dyn StorageRead>, Error> {
        self.storage.open_read(path)
    }

    fn append(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        self.storage.append(path)
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        self.storage.remove(path)
    }

    fn watch(&self, paths: &[&str]) -> Result<Option<Box<dyn ChangeSignal>>, Error> {
        self.storage.watch(paths)
    }

    fn label(&self) -> BoxFuture<'_, Result<Option<String>, Error>> {
        self.storage.label()
    }

    fn set_label<'a>(&'a self, label: Option<&'a str>) -> BoxFuture<'a, Result<(), Error>> {
        self.storage.set_label(label)
    }

    fn install_file<'a>(
        &'a self,
        source: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        self.storage.install_file(source, dest)
    }
}

impl MemoryCard for Playstation2MemoryCard {
    fn view(&self, primary: &[SaveDepotId], format: &str) -> Result<MemoryCardView, Error> {
        Self::validate_capacity(CAPACITY_BLOCKS)?;
        let saves = self.project_saves(primary, CAPACITY_BLOCKS)?;
        let projected = saves
            .iter()
            .map(|save| save.name().into_owned())
            .collect::<Vec<_>>();
        let storage: Arc<dyn MountableStorage> = match format {
            "raw" => {
                let card = self.clone();
                let owned = Arc::new(Mutex::new(projected));
                Arc::new(RawMemoryStorage::new(
                    self.storage.clone(),
                    CAPACITY_BLOCKS,
                    saves,
                    move |saves| {
                        let previous = owned.lock().clone();
                        card.reconcile(&previous, saves)?;
                        *owned.lock() = saves.iter().map(|save| save.name().into_owned()).collect();
                        Ok(())
                    },
                )?)
            }
            "pcsx2" => {
                let card = self.clone();
                let owned = Arc::new(Mutex::new(projected));
                Arc::new(Pcsx2MemoryStorage::new(
                    self.storage.clone(),
                    CAPACITY_BLOCKS,
                    saves,
                    move |saves| {
                        let previous = owned.lock().clone();
                        card.reconcile(&previous, saves)?;
                        *owned.lock() = saves.iter().map(|save| save.name().into_owned()).collect();
                        Ok(())
                    },
                )?)
            }
            _ => {
                return Err(Error::Unsupported(format!(
                    "PlayStation 2 memory cards do not support the '{format}' format"
                )));
            }
        };
        MemoryCardView::new(storage, "")
    }
}

impl SaveGameSink for Playstation2MemoryCard {
    fn infer_save_depot(&self, package: &Package) -> SaveDepotId {
        let serial = package
            .serial()
            .expect("a PlayStation 2 package must expose a product serial");
        SaveDataHandle::depot_id_from_serial(serial)
    }

    fn enumerate_saves(&self) -> impl Iterator<Item = SaveDescriptor> + '_ {
        let mut seen = HashSet::new();
        self.saves()
            .into_iter()
            .filter(|save| seen.insert(save.depot_id().0))
            .filter_map(|save| self.describe_save(&save).ok())
            .collect::<Vec<_>>()
            .into_iter()
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
        blobs: impl Stream<Item = SaveBlob> + Send + 'a,
    ) -> BoxFuture<'a, Result<SaveDescriptor, Error>> {
        Box::pin(async move {
            let mut blobs = std::pin::pin!(blobs);
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
                if parsed.name().as_ref() != relative.name {
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
        let activity = self.storage.watch(&[SAVE_ROOT])?.ok_or_else(|| {
            Error::Unsupported("PlayStation 2 storage cannot watch saves".to_owned())
        })?;
        let signal = SaveSignal {
            card: self.clone(),
            saves: saves.to_vec(),
            activity,
            last: Mutex::new(self.snapshot(saves)),
        };
        Ok(SaveWatcher::new(saves.to_vec(), Box::new(signal)))
    }
}

struct SaveSignal {
    card: Playstation2MemoryCard,
    saves: Vec<SaveDepotId>,
    activity: Box<dyn ChangeSignal>,
    last: Mutex<Vec<(String, u64, Timestamp)>>,
}

impl ChangeSignal for SaveSignal {
    fn changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            loop {
                self.activity.changed().await;
                tokio::time::sleep(Duration::from_millis(50)).await;
                let current = self.card.snapshot(&self.saves);
                let mut last = self.last.lock();
                if *last != current {
                    *last = current;
                    return;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirent(name: &str, mode: u32, attributes: u32, second: u8) -> Vec<u8> {
        let mut raw = vec![0; 512];
        raw[..4].copy_from_slice(&mode.to_le_bytes());
        raw[9] = second;
        raw[13] = 8;
        raw[14..16].copy_from_slice(&2026u16.to_le_bytes());
        raw[0x20..0x24].copy_from_slice(&attributes.to_le_bytes());
        raw[0x40..0x40 + name.len()].copy_from_slice(name.as_bytes());
        raw
    }

    #[tokio::test]
    async fn raw_and_pcsx2_views_reconcile_through_the_canonical_card() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(directory.path()).await.unwrap();
        let card = Playstation2MemoryCard::new_empty(storage, Uuid::now_v7(), None)
            .await
            .unwrap();
        card.write_save(
            &Save::from_files("BASLUS-00001", &[], [("data.bin", &[][..], &b"first"[..])]).unwrap(),
        )
        .unwrap();
        assert!(card.exists("card/BASLUS-00001.psu"));

        let pcsx2 = card.view(&[], "pcsx2").unwrap();
        assert_eq!(pcsx2.kind(), EntryKind::Directory);
        assert!(pcsx2.storage().exists("BASLUS-00001/data.bin"));
        assert!(pcsx2.storage().exists("_pcsx2_superblock"));
        assert!(!card.exists("card/_pcsx2_superblock"));

        let watcher = card.watch_saves(&[]).unwrap();
        let notification = tokio::spawn(async move { watcher.changed().await });
        tokio::task::yield_now().await;
        pcsx2.storage().create_dir_all("BASLUS-00002").unwrap();
        pcsx2
            .storage()
            .create("BASLUS-00002/save.dat")
            .unwrap()
            .write_all(b"folder write")
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), notification)
            .await
            .expect("folder write should publish a save notification")
            .unwrap();
        let written = card
            .read_save(&SaveDataHandle::from_name("BASLUS-00002").unwrap())
            .unwrap();
        assert_eq!(written.files().next().unwrap().data(), b"folder write");

        pcsx2
            .storage()
            .create("index.tmp")
            .unwrap()
            .write_all(b"projection metadata")
            .unwrap();
        pcsx2.storage().rename("index.tmp", "_pcsx2_index").unwrap();
        assert!(pcsx2.storage().exists("_pcsx2_index"));
        assert!(!card.exists("card/index.tmp"));
        assert!(!card.exists("card/_pcsx2_index"));

        pcsx2.storage().create_dir_all("BASLUS-DELETE").unwrap();
        pcsx2.storage().create("BASLUS-DELETE/save.dat").unwrap();
        pcsx2
            .storage()
            .rename("BASLUS-DELETE", "_pcsx2_deleted_BASLUS-DELETE")
            .unwrap();
        assert!(!card.exists("card/BASLUS-DELETE.psu"));

        let raw = card.view(&[], "raw").unwrap();
        assert_eq!(raw.kind(), EntryKind::File);
        let mut bytes = Vec::new();
        raw.storage()
            .open_read("")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        let projected = raw::card::MemoryCard::new(&bytes, CAPACITY_BLOCKS).unwrap();
        let saves = projected.saves().unwrap();
        assert_eq!(saves.len(), 2);
        assert_eq!(saves[0].files().next().unwrap().data(), b"first");
        assert_eq!(saves[1].files().next().unwrap().data(), b"folder write");

        let replacement = Save::from_files(
            "BASLUS-00001",
            &[],
            [("data.bin", &[][..], &b"raw write"[..])],
        )
        .unwrap();
        let replacement =
            raw::card::MemoryCard::format(CAPACITY_BLOCKS, &[replacement], true).unwrap();
        raw.storage()
            .truncate("", replacement.len() as u64)
            .unwrap();
        raw.storage()
            .open("")
            .unwrap()
            .write_all(&replacement)
            .unwrap();
        raw.storage().sync("").unwrap();

        let written = card
            .read_save(&SaveDataHandle::from_name("BASLUS-00001").unwrap())
            .unwrap();
        assert_eq!(written.files().next().unwrap().data(), b"raw write");
        assert!(!card.exists("card/BASLUS-00002.psu"));
        let pcsx2 = card.view(&[], "pcsx2").unwrap();
        let mut payload = Vec::new();
        pcsx2
            .storage()
            .open_read("BASLUS-00001/data.bin")
            .unwrap()
            .read_to_end(&mut payload)
            .unwrap();
        assert_eq!(payload, b"raw write");
    }

    #[tokio::test]
    async fn dirent_metadata_survives_reopening_the_card() {
        let directory = tempfile::tempdir().unwrap();
        let storage = DirectoryStorage::new(directory.path()).await.unwrap();
        let card = Playstation2MemoryCard::new_empty(storage, Uuid::now_v7(), None)
            .await
            .unwrap();
        let raw = card.view(&[], "raw").unwrap();

        let save_dirent = dirent("BASLUS-12345", 0x8427, 0x1234_5678, 17);
        let file_dirent = dirent("save.dat", 0x8497, 0x8765_4321, 31);
        let replacement = Save::from_files(
            "BASLUS-12345",
            &save_dirent,
            [("save.dat", file_dirent.as_slice(), &b"persistent"[..])],
        )
        .unwrap();
        let replacement =
            raw::card::MemoryCard::format(CAPACITY_BLOCKS, &[replacement], true).unwrap();
        raw.storage()
            .truncate("", replacement.len() as u64)
            .unwrap();
        raw.storage()
            .open("")
            .unwrap()
            .write_all(&replacement)
            .unwrap();
        raw.storage().sync("").unwrap();
        assert!(directory.path().join("card/BASLUS-12345.psu").is_file());
        assert!(!directory.path().join("._dirent").exists());

        drop(raw);
        drop(card);
        let reopened =
            Playstation2MemoryCard::open(DirectoryStorage::new(directory.path()).await.unwrap())
                .await
                .unwrap();
        let pcsx2 = reopened.view(&[], "pcsx2").unwrap();
        let metadata_path = "BASLUS-12345/_pcsx2_meta/save.dat";
        let mut projected_metadata = Vec::new();
        pcsx2
            .storage()
            .open_read(metadata_path)
            .unwrap()
            .read_to_end(&mut projected_metadata)
            .unwrap();
        assert_eq!(projected_metadata[0x40..0x60], file_dirent[0x40..0x60]);
        projected_metadata[0x20..0x24].copy_from_slice(&0xfeed_beefu32.to_le_bytes());
        pcsx2
            .storage()
            .open(metadata_path)
            .unwrap()
            .write_all(&projected_metadata)
            .unwrap();
        pcsx2.storage().sync(metadata_path).unwrap();

        let projected = reopened.view(&[], "raw").unwrap();
        let mut image = Vec::new();
        projected
            .storage()
            .open_read("")
            .unwrap()
            .read_to_end(&mut image)
            .unwrap();
        let projected = raw::card::MemoryCard::new(&image, CAPACITY_BLOCKS).unwrap();
        let saves = projected.saves().unwrap();
        let persisted_save = saves[0].raw_dirent();
        let persisted_file = saves[0].files().next().unwrap();
        let persisted_file = persisted_file.raw_dirent();
        for range in [0..4, 8..16, 0x20..0x24, 0x40..0x60] {
            assert_eq!(persisted_save[range.clone()], save_dirent[range.clone()]);
            if range == (0x20..0x24) {
                assert_eq!(persisted_file[range], 0xfeed_beefu32.to_le_bytes());
            } else {
                assert_eq!(persisted_file[range.clone()], file_dirent[range]);
            }
        }
    }
}
