use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use jiff::Timestamp;
use parking_lot::Mutex;

use crate::impls::memorycard::{RawMemoryCardImage, ReconcileWorker};
use crate::{
    BoxFuture, DirectoryStorage, EntryKind, Error, InstallResult, Metadata, MountableStorage,
    ReadDir, Storage, StorageFile, StorageRead,
};

use super::psu::Save;

pub(super) mod card;

type Reconcile = dyn for<'save> Fn(&[Save<'save>]) -> Result<(), Error> + Send + Sync;

/// A raw PS2 memory-card image exposed as one root file.
///
/// Structured saves are encoded when the view is created and decoded again at
/// each synchronization boundary. The codec is deliberately confined here so
/// it can be replaced without changing the storage or VFS interfaces.
pub(super) struct RawMemoryStorage {
    backing: Arc<DirectoryStorage>,
    image: Arc<CardImage>,
    worker: ReconcileWorker,
}

struct CardImage {
    bytes: Mutex<Vec<u8>>,
    blocks: u32,
    reconcile: Arc<Reconcile>,
}

impl CardImage {
    fn reconcile(&self) -> Result<(), Error> {
        let image = self.bytes.lock().clone();
        let card = match card::MemoryCard::new(&image, self.blocks) {
            Ok(card) => card,
            Err(_) => return Ok(()),
        };
        let mut saves = match card.saves() {
            Ok(saves) => saves,
            Err(_) => return Ok(()),
        };
        let had_incomplete = saves.iter().any(|save| save.files().len() == 0);
        saves.retain(|save| save.files().len() != 0);
        if had_incomplete && saves.is_empty() {
            return Ok(());
        }
        (self.reconcile)(&saves)
    }
}

impl RawMemoryStorage {
    pub(super) fn new<'a>(
        backing: Arc<DirectoryStorage>,
        capacity_blocks: u32,
        saves: impl IntoIterator<Item = Save<'a>>,
        reconcile: impl for<'save> Fn(&[Save<'save>]) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let saves = saves.into_iter().collect::<Vec<_>>();
        let image = Arc::new(CardImage {
            bytes: Mutex::new(card::MemoryCard::format(capacity_blocks, &saves, true)?),
            blocks: capacity_blocks,
            reconcile: Arc::new(reconcile),
        });
        let background = image.clone();
        let worker =
            ReconcileWorker::spawn("ps2-memory-card", "PlayStation 2 memory card", move || {
                background.reconcile()
            })?;
        Ok(Self {
            backing,
            image,
            worker,
        })
    }

    fn handle(&self, path: &str) -> Result<MemoryHandle, Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        Ok(MemoryHandle {
            image: self.image.clone(),
            worker: self.worker.clone(),
            position: 0,
        })
    }
}

impl fmt::Debug for RawMemoryStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Ps2RawMemoryStorage")
            .finish_non_exhaustive()
    }
}

impl Storage for RawMemoryStorage {
    fn exists(&self, path: &str) -> bool {
        path.is_empty()
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        Ok(Metadata {
            kind: EntryKind::File,
            len: self.image.bytes.lock().len() as u64,
            mtime: Timestamp::now(),
        })
    }

    fn read_dir(&self, _path: &str, _recursive: bool) -> Result<ReadDir<'_>, Error> {
        Err(Error::Unsupported(
            "a raw memory-card view is a file".to_owned(),
        ))
    }

    fn create_dir_all(&self, _path: &str) -> Result<(), Error> {
        Err(Error::Unsupported(
            "a raw memory-card view is a file".to_owned(),
        ))
    }

    fn create(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        self.truncate(path, 0)?;
        Ok(Box::new(self.handle(path)?))
    }

    fn open(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        Ok(Box::new(self.handle(path)?))
    }

    fn open_read(&self, path: &str) -> Result<Box<dyn StorageRead>, Error> {
        Ok(Box::new(self.handle(path)?))
    }

    fn append(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
        let mut handle = self.handle(path)?;
        handle.position = self.image.bytes.lock().len() as u64;
        Ok(Box::new(handle))
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        self.image.bytes.lock().clear();
        Ok(())
    }

    fn label(&self) -> BoxFuture<'_, Result<Option<String>, Error>> {
        self.backing.label()
    }

    fn set_label<'a>(&'a self, label: Option<&'a str>) -> BoxFuture<'a, Result<(), Error>> {
        self.backing.set_label(label)
    }

    fn install_file<'a>(
        &'a self,
        _source: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        _dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        Box::pin(async {
            Err(Error::Unsupported(
                "files cannot be installed inside a raw memory-card view".to_owned(),
            ))
        })
    }
}

impl MountableStorage for RawMemoryStorage {
    fn rename(&self, _from: &str, _to: &str) -> Result<(), Error> {
        Err(Error::Unsupported(
            "a raw memory-card view has one fixed entry".to_owned(),
        ))
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        let len = usize::try_from(len)
            .map_err(|_| Error::Unsupported("raw memory-card image is too large".to_owned()))?;
        self.image.bytes.lock().resize(len, 0);
        Ok(())
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        self.worker.reconcile()
    }
}

struct MemoryHandle {
    image: Arc<CardImage>,
    worker: ReconcileWorker,
    position: u64,
}

impl Read for MemoryHandle {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let bytes = self.image.bytes.lock();
        let start = usize::try_from(self.position)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let copied = output.len().min(bytes.len() - start);
        output[..copied].copy_from_slice(&bytes[start..start + copied]);
        self.position += copied as u64;
        Ok(copied)
    }
}

impl Write for MemoryHandle {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        let start = usize::try_from(self.position)
            .map_err(|_| std::io::Error::other("memory-card offset is too large"))?;
        {
            let mut bytes = self.image.bytes.lock();
            let end = start
                .checked_add(input.len())
                .ok_or_else(|| std::io::Error::other("memory-card write is too large"))?;
            if end > bytes.len() {
                bytes.resize(end, 0);
            }
            bytes[start..end].copy_from_slice(input);
            self.position = end as u64;
        }

        self.worker.schedule();
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.worker.schedule();
        Ok(())
    }
}

impl Seek for MemoryHandle {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let len = self.image.bytes.lock().len() as i128;
        let position = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::End(offset) => len + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        if !(0..=i128::from(u64::MAX)).contains(&position) {
            return Err(std::io::Error::other("invalid memory-card seek"));
        }
        self.position = position as u64;
        Ok(self.position)
    }
}
