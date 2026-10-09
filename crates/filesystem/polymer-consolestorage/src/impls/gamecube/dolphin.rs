use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

use super::{
    GameCubeMemoryCard,
    gci::{self, Region, Save},
};
use crate::impls::memorycard::WriteBack;
use crate::{
    BoxFuture, Error, InstallResult, MemoryStorage, Metadata, MountableStorage, ReadDir, Storage,
    StorageFile,
};

pub(super) struct DolphinMemoryStorage {
    storage: MemoryStorage,
    write_back: WriteBack<Vec<u8>>,
    region: Region,
    view_identities: HashMap<String, Vec<u8>>,
}

impl DolphinMemoryStorage {
    fn saves(&self) -> Vec<(Vec<u8>, Option<Save<'static>>)> {
        let mut saves = Vec::new();
        for (path, bytes) in self.storage.snapshot_files() {
            if !path.to_ascii_lowercase().ends_with(".gci") {
                continue;
            }
            let header = bytes
                .first_chunk::<{ gci::DIRENT_SIZE }>()
                .map(Save::dirent_identity);
            match Save::new(bytes) {
                Ok(save) => saves.push((save.identity(), Some(save))),
                Err(_) => saves.extend(
                    header
                        .into_iter()
                        .chain(self.view_identities.get(&path).cloned())
                        .map(|identity| (identity, None)),
                ),
            }
        }
        saves
    }

    pub(super) fn new(
        card: GameCubeMemoryCard,
        region: Region,
        saves: &[Save<'_>],
    ) -> Result<Self, Error> {
        let mut entries = BTreeMap::new();
        let mut view_identities = HashMap::new();
        for save in saves {
            let mut name = save.file_name();
            while entries.contains_key(&name) {
                name.insert(name.len() - 4, '0');
            }
            view_identities.insert(name.clone(), save.identity());
            entries.insert(name, save.as_bytes().to_vec());
        }
        Ok(Self {
            storage: MemoryStorage::writable(entries)?,
            write_back: card.write_back(saves),
            region,
            view_identities,
        })
    }
}

impl crate::ReconciliableStorage for DolphinMemoryStorage {
    fn reconcile(&self) -> Result<(), Error> {
        GameCubeMemoryCard::reconcile(&self.write_back, self.region, &self.saves())
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
