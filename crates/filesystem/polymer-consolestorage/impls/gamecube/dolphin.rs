use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use super::{
    GameCubeMemoryCard, SAVE_ROOT,
    gci::{self, Region, Save},
};
use crate::{
    BoxFuture, Error, InstallResult, MemoryStorage, Metadata, MountableStorage, ReadDir, Storage,
    StorageFile,
};

pub(super) struct DolphinMemoryStorage {
    storage: MemoryStorage,
    card: GameCubeMemoryCard,
    region: Region,
    projected_paths: HashMap<Vec<u8>, String>,
}

impl DolphinMemoryStorage {
    fn saves(&self) -> Result<Option<Vec<Save<'static>>>, Error> {
        let entries = self.storage.snapshot_files();
        let mut identities = HashSet::new();
        let mut saves = Vec::new();
        for (path, bytes) in entries {
            if !path.to_ascii_lowercase().ends_with(".gci") {
                continue;
            }
            if bytes.len() < gci::DIRENT_SIZE {
                return Ok(None);
            }
            let blocks = u16::from_be_bytes([bytes[0x38], bytes[0x39]]);
            let expected = gci::DIRENT_SIZE + usize::from(blocks) * gci::BLOCK_SIZE;
            if bytes.len() < expected {
                return Ok(None);
            }
            let save = Save::new(bytes).map_err(|error| Error::MalformedMetadata {
                file: path.clone(),
                reason: error.to_string(),
            })?;
            if !identities.insert(save.identity()) {
                return Err(Error::Unsupported(
                    "a Dolphin memory-card folder contains duplicate save identities".to_owned(),
                ));
            }
            saves.push(save);
        }
        Ok(Some(saves))
    }

    pub(super) fn new(
        card: GameCubeMemoryCard,
        region: Region,
        saves: &[Save<'_>],
    ) -> Result<Self, Error> {
        let projected_paths = saves
            .iter()
            .map(|save| (save.identity(), format!("{SAVE_ROOT}/{}", save.file_name())))
            .collect();
        let mut entries = BTreeMap::new();
        for save in saves {
            let mut name = save.file_name();
            while entries.contains_key(&name) {
                name.insert(name.len() - 4, '0');
            }
            entries.insert(name, save.as_bytes().to_vec());
        }
        Ok(Self {
            storage: MemoryStorage::writable(entries)?,
            card,
            region,
            projected_paths,
        })
    }
}

impl crate::ReconciliableStorage for DolphinMemoryStorage {
    fn reconcile(&self) -> Result<(), Error> {
        if let Some(saves) = self.saves()? {
            self.card
                .reconcile(self.region, &self.projected_paths, &saves)?;
        }
        Ok(())
    }

    fn filter(_path: &str) -> bool {
        true
    }
}

impl fmt::Debug for DolphinMemoryStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DolphinMemoryStorage")
            .field("storage", &self.storage)
            .finish_non_exhaustive()
    }
}

impl Storage for DolphinMemoryStorage {
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
        if path.is_empty() {
            Ok(())
        } else {
            Err(Error::Unsupported(
                "Dolphin memory-card folders cannot contain directories".to_owned(),
            ))
        }
    }

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        if path.is_empty() || path.contains('/') {
            return Err(Error::InvalidPath(path.to_owned()));
        }
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
        if dest.is_empty() || dest.contains('/') {
            let error = Error::InvalidPath(dest.to_owned());
            return Box::pin(async move { Err(error) });
        }
        self.storage.install_file(source, dest)
    }
}

impl MountableStorage for DolphinMemoryStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        if from.is_empty() || to.is_empty() || to.contains('/') {
            return Err(Error::InvalidPath(to.to_owned()));
        }
        if to.to_ascii_lowercase().ends_with(".deleted") {
            self.storage.metadata(from)?;
            self.storage.remove(from)
        } else {
            self.storage.rename(from, to)
        }
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        self.storage.truncate(path, len)
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        self.storage.sync(path)
    }
}
