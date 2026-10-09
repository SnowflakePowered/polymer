use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use encoding_rs::SHIFT_JIS;

use crate::impls::memorycard::WriteBack;
use crate::{
    BoxFuture, Error, InstallResult, MemoryStorage, Metadata, MountableStorage, ReadDir, Storage,
    StorageFile,
};

use super::psu::Save;
use super::{DecodedSaves, Playstation2MemoryCard, raw};

const SUPERBLOCK: &str = "_pcsx2_superblock";
const DELETED_PREFIX: &str = "_pcsx2_deleted_";

/// An in-memory PCSX2 folder-card projection of PSU-backed saves.
pub(super) struct Pcsx2MemoryStorage {
    storage: MemoryStorage,
    write_back: WriteBack<Vec<u8>>,
    capacity_blocks: u32,
}

impl Pcsx2MemoryStorage {
    pub(super) fn new<'a>(
        card: Playstation2MemoryCard,
        capacity_blocks: u32,
        saves: impl IntoIterator<Item = Save<'a>>,
    ) -> Result<Self, Error> {
        let superblock = raw::card::MemoryCard::formatted_superblock(capacity_blocks)?;
        let storage = MemoryStorage::writable([(SUPERBLOCK.to_owned(), superblock)])?;
        let mut projected = Vec::new();
        for save in saves {
            Self::project_save(&storage, &save)?;
            projected.push(save.name_bytes().to_vec());
        }
        Ok(Self {
            storage,
            write_back: card.write_back(projected.iter().map(Vec::as_slice)),
            capacity_blocks,
        })
    }

    fn project_save(storage: &MemoryStorage, save: &Save<'_>) -> Result<(), Error> {
        let save_name = save.name().into_owned();
        storage
            .create(&format!("{save_name}/_pcsx2_meta_directory"))?
            .write_all_at(0, save.raw_dirent())?;
        let metadata = format!("{save_name}/_pcsx2_meta");
        storage.create_dir_all(&metadata)?;
        let mut index = String::from("{\"$ROOT\":{\"timeCreated\":0,\"timeModified\":0}");
        for (order, file) in save.files().enumerate() {
            let file_name = file.name().into_owned();
            storage
                .create(&format!("{save_name}/{file_name}"))?
                .write_all_at(0, file.data())?;
            storage
                .create(&format!("{metadata}/{file_name}"))?
                .write_all_at(0, file.raw_dirent())?;
            index.push(',');
            index.push_str(&serde_json::to_string(&file_name).expect("a string serializes"));
            index.push_str(":{\"order\":");
            index.push_str(&order.to_string());
            index.push_str(",\"timeCreated\":0,\"timeModified\":0}");
        }
        index.push('}');
        storage
            .create(&format!("{save_name}/_pcsx2_index"))?
            .write_all_at(0, index.as_bytes())?;
        Ok(())
    }

    fn is_private(path: &str) -> bool {
        path.split('/').any(|part| part.starts_with("_pcsx2_"))
    }

    fn dirent<'a>(entries: &'a BTreeMap<String, Vec<u8>>, path: &str, name: &str) -> &'a [u8] {
        let bytes = entries.get(path).map(Vec::as_slice).unwrap_or_default();
        let Some(raw_name) = bytes.get(0x40..0x60) else {
            return &[];
        };
        let end = raw_name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(raw_name.len());
        if raw_name[..end] == *name.as_bytes() {
            bytes
        } else {
            &[]
        }
    }

    fn saves(&self) -> Option<DecodedSaves<'static>> {
        let entries = self.storage.snapshot_files();
        raw::card::MemoryCard::validate_superblock(entries.get(SUPERBLOCK)?, self.capacity_blocks)
            .ok()?;
        let roots = entries
            .keys()
            .filter_map(|path| {
                let (root, _) = path.split_once('/')?;
                (!Self::is_private(root)).then_some(root)
            })
            .collect::<BTreeSet<_>>();
        Some(
            roots
                .into_iter()
                .map(|name| {
                    let native = SHIFT_JIS.encode(name).0.into_owned();
                    (native, Self::save(&entries, name))
                })
                .collect(),
        )
    }

    fn save(entries: &BTreeMap<String, Vec<u8>>, name: &str) -> Option<Save<'static>> {
        let prefix = format!("{name}/");
        let mut files = entries
            .iter()
            .filter_map(|(path, bytes)| {
                let child = path.strip_prefix(&prefix)?;
                (!child.contains('/') && !Self::is_private(child)).then(|| {
                    (
                        child.to_owned(),
                        Self::dirent(entries, &format!("{name}/_pcsx2_meta/{child}"), child),
                        bytes.as_slice(),
                    )
                })
            })
            .collect::<Vec<_>>();
        if files.is_empty() {
            return None;
        }
        let index = entries
            .get(&format!("{name}/_pcsx2_index"))
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).ok());
        files.sort_by_cached_key(|file| {
            let order = index
                .as_ref()
                .and_then(|index| index.get(&file.0)?.get("order")?.as_u64())
                .unwrap_or(u64::MAX);
            (order, file.0.clone())
        });
        let dirent = Self::dirent(entries, &format!("{name}/_pcsx2_meta_directory"), name);
        Save::from_files(
            name,
            dirent,
            files
                .iter()
                .map(|(name, dirent, data)| (name.as_str(), *dirent, *data)),
        )
        .ok()
    }
}

impl crate::ReconciliableStorage for Pcsx2MemoryStorage {
    fn reconcile(&self) -> Result<(), Error> {
        let Some(saves) = self.saves() else {
            return Ok(());
        };
        Playstation2MemoryCard::reconcile(&self.write_back, saves)
    }

    fn filter(path: &str) -> bool {
        path.contains('/')
    }
}

impl fmt::Debug for Pcsx2MemoryStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Pcsx2MemoryStorage")
            .field("storage", &self.storage)
            .finish_non_exhaustive()
    }
}

impl Storage for Pcsx2MemoryStorage {
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

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        self.storage.create(path)
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        self.storage.open(path, write)
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        self.storage.remove(path)
    }

    fn install_file<'a>(
        &'a self,
        source: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        self.storage.install_file(source, dest)
    }
}

impl MountableStorage for Pcsx2MemoryStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        if to
            .rsplit('/')
            .next()
            .is_some_and(|name| name.starts_with(DELETED_PREFIX))
        {
            return self.storage.remove_tree(from);
        }
        self.storage.rename(from, to)
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        self.storage.truncate(path, len)
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        self.storage.sync(path)
    }
}
