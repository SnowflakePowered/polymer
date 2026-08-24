use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::sync::Arc;

use jiff::Timestamp;
use parking_lot::Mutex;
use tokio::io::AsyncReadExt;

use super::gci::{self, Save};
use crate::impls::memorycard::{MemoryFile, ReconcileWorker};
use crate::{
    BoxFuture, DirEntry, DirectoryStorage, EntryKind, Error, InstallResult, Metadata,
    MountableStorage, ReadDir, Storage, StorageFile,
};

type Reconcile = dyn for<'save> Fn(&[Save<'save>]) -> Result<(), Error> + Send + Sync;

struct Projection {
    entries: Mutex<BTreeMap<String, MemoryFile>>,
    reconcile: Box<Reconcile>,
}

impl Projection {
    fn saves(&self) -> Result<Option<Vec<Save<'static>>>, Error> {
        let entries = self.entries.lock();
        let mut identities = HashSet::new();
        let mut saves = Vec::new();
        for (path, entry) in entries.iter() {
            if !path.to_ascii_lowercase().ends_with(".gci") {
                continue;
            }
            let bytes = entry.bytes();
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

    fn commit(&self) -> Result<(), Error> {
        if let Some(saves) = self.saves()? {
            (self.reconcile)(&saves)?;
        }
        Ok(())
    }
}

pub(super) struct DolphinMemoryStorage {
    backing: Arc<DirectoryStorage>,
    projection: Arc<Projection>,
    worker: ReconcileWorker,
}

impl DolphinMemoryStorage {
    pub(super) fn new(
        backing: Arc<DirectoryStorage>,
        saves: &[Save<'_>],
        reconcile: impl for<'save> Fn(&[Save<'save>]) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let mut entries = BTreeMap::new();
        for save in saves {
            let mut name = save.file_name();
            while entries.contains_key(&name) {
                name.insert(name.len() - 4, '0');
            }
            entries.insert(name, MemoryFile::new(save.as_bytes().to_vec()));
        }
        let projection = Arc::new(Projection {
            entries: Mutex::new(entries),
            reconcile: Box::new(reconcile),
        });
        let background = Arc::downgrade(&projection);
        let worker = ReconcileWorker::spawn(
            "snowglass-gci-writeback",
            "Dolphin memory card",
            move || {
                background
                    .upgrade()
                    .map_or(Ok(()), |projection| projection.commit())
            },
        )?;
        Ok(Self {
            backing,
            projection,
            worker,
        })
    }

    fn entry(&self, path: &str) -> Result<MemoryFile, Error> {
        self.projection
            .entries
            .lock()
            .get(path)
            .cloned()
            .ok_or_else(|| Error::not_found(path))
    }
}

impl fmt::Debug for DolphinMemoryStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DolphinMemoryStorage")
            .field("backing", &self.backing)
            .finish_non_exhaustive()
    }
}

impl Storage for DolphinMemoryStorage {
    fn exists(&self, path: &str) -> bool {
        path.is_empty() || self.projection.entries.lock().contains_key(path)
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        if path.is_empty() {
            return Ok(Metadata {
                kind: EntryKind::Directory,
                len: 0,
                mtime: Timestamp::now(),
            });
        }
        Ok(self.entry(path)?.metadata())
    }

    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        if recursive {
            return ReadDir::recursive(self, path);
        }
        let entries = self
            .projection
            .entries
            .lock()
            .iter()
            .map(|(name, entry)| DirEntry {
                name: name.clone(),
                path: name.clone(),
                metadata: entry.metadata(),
            })
            .collect::<Vec<_>>();
        Ok(ReadDir::surface(entries.into_iter()))
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
        let entry = MemoryFile::new(Vec::new());
        self.projection
            .entries
            .lock()
            .insert(path.to_owned(), entry.clone());
        Ok(entry.handle(self.worker.clone(), true, true))
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        Ok(self.entry(path)?.handle(self.worker.clone(), write, true))
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        self.projection.entries.lock().remove(path);
        self.worker.schedule();
        Ok(())
    }

    fn install_file<'a>(
        &'a self,
        mut source: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        let dest = dest.to_owned();
        Box::pin(async move {
            let mut bytes = Vec::new();
            source
                .read_to_end(&mut bytes)
                .await
                .map_err(|source| Error::Io {
                    path: dest.clone(),
                    source,
                })?;
            let result = InstallResult {
                blake3: *blake3::hash(&bytes).as_bytes(),
                byte_length: bytes.len() as u64,
            };
            self.create(&dest)?.write_all_at(0, &bytes)?;
            Ok(result)
        })
    }
}

impl MountableStorage for DolphinMemoryStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        let mut entries = self.projection.entries.lock();
        let entry = entries.remove(from).ok_or_else(|| Error::not_found(from))?;
        if !to.to_ascii_lowercase().ends_with(".deleted") {
            entries.insert(to.to_owned(), entry);
        }
        drop(entries);
        self.worker.schedule();
        Ok(())
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        self.entry(path)?
            .handle(self.worker.clone(), true, true)
            .set_len(len)
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        if !path.is_empty() {
            self.entry(path)?;
        }
        self.worker.reconcile()
    }

    fn finish(&self, path: &str) -> Result<(), Error> {
        if !path.is_empty() {
            self.entry(path)?;
        }
        self.worker.shutdown()
    }
}
