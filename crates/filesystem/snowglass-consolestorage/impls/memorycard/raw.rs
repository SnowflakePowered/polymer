use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use jiff::Timestamp;
use parking_lot::Mutex;

use super::ReconcileWorker;
use crate::{
    BoxFuture, EntryKind, Error, InstallResult, MemoryCard, Metadata, MountableStorage, ReadDir,
    Reconcile, Storage, StorageFile, StorageRead, StorageStream,
};

pub(crate) struct CardImage {
    bytes: Mutex<Vec<u8>>,
    reconcile: Reconcile,
}

impl CardImage {
    fn reconcile(&self) -> Result<(), Error> {
        let bytes = self.bytes.lock().clone();
        (self.reconcile)(&bytes)
    }
}

pub(crate) struct MemoryCardStorage<T: MemoryCard + 'static> {
    backing: T,
    image: Arc<CardImage>,
    worker: ReconcileWorker,
}

impl<T: MemoryCard + 'static> MemoryCardStorage<T> {
    pub(crate) fn new(
        card: T,
        primary: &[crate::savegame::SaveDepotId],
        bytes: Vec<u8>,
        thread_name: &str,
        display_name: &str,
    ) -> Result<Self, Error> {
        let reconcile = card.reconcile(primary)?;
        let image = Arc::new(CardImage {
            bytes: Mutex::new(bytes),
            reconcile,
        });
        let background = image.clone();
        let worker =
            ReconcileWorker::spawn(thread_name, display_name, move || background.reconcile())?;
        Ok(Self {
            backing: card,
            image,
            worker,
        })
    }

    fn handle(&self, path: &str) -> Result<Handle, Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        Ok(Handle {
            image: self.image.clone(),
            worker: self.worker.clone(),
            position: 0,
        })
    }
}

impl<T: MemoryCard + 'static> fmt::Debug for MemoryCardStorage<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryCardStorage")
            .finish_non_exhaustive()
    }
}

impl<T: MemoryCard + 'static> Storage for MemoryCardStorage<T> {
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

    fn create(&self, path: &str) -> Result<Box<dyn StorageStream>, Error> {
        self.truncate(path, 0)?;
        Ok(Box::new(self.handle(path)?))
    }

    fn open_read(&self, path: &str) -> Result<Box<dyn StorageRead>, Error> {
        Ok(Box::new(self.handle(path)?))
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        Ok(Arc::new(RawStorageFile {
            image: self.image.clone(),
            worker: self.worker.clone(),
            writable: write,
        }))
    }

    fn append(&self, path: &str) -> Result<Box<dyn StorageStream>, Error> {
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
        self.backing.storage().label()
    }

    fn set_label<'a>(&'a self, label: Option<&'a str>) -> BoxFuture<'a, Result<(), Error>> {
        self.backing.storage().set_label(label)
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

struct RawStorageFile {
    image: Arc<CardImage>,
    worker: ReconcileWorker,
    writable: bool,
}

impl StorageFile for RawStorageFile {
    fn metadata(&self) -> Result<Metadata, Error> {
        Ok(Metadata {
            kind: EntryKind::File,
            len: self.image.bytes.lock().len() as u64,
            mtime: Timestamp::now(),
        })
    }

    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, Error> {
        let bytes = self.image.bytes.lock();
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let copied = output.len().min(bytes.len() - start);
        output[..copied].copy_from_slice(&bytes[start..start + copied]);
        Ok(copied)
    }

    fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, Error> {
        if !self.writable {
            return Err(Error::Unsupported(
                "raw memory-card handle is read-only".to_owned(),
            ));
        }
        let start = usize::try_from(offset)
            .map_err(|_| Error::Unsupported("memory-card offset is too large".to_owned()))?;
        let end = start
            .checked_add(input.len())
            .ok_or_else(|| Error::Unsupported("memory-card write is too large".to_owned()))?;
        let mut bytes = self.image.bytes.lock();
        if end > bytes.len() {
            bytes.resize(end, 0);
        }
        bytes[start..end].copy_from_slice(input);
        drop(bytes);
        self.worker.schedule();
        Ok(input.len())
    }

    fn set_len(&self, len: u64) -> Result<(), Error> {
        if !self.writable {
            return Err(Error::Unsupported(
                "raw memory-card handle is read-only".to_owned(),
            ));
        }
        let len = usize::try_from(len)
            .map_err(|_| Error::Unsupported("raw memory-card image is too large".to_owned()))?;
        self.image.bytes.lock().resize(len, 0);
        self.worker.schedule();
        Ok(())
    }

    fn flush(&self) -> Result<(), Error> {
        self.worker.schedule();
        Ok(())
    }

    fn sync(&self) -> Result<(), Error> {
        self.worker.reconcile()
    }
}

impl<T: MemoryCard + 'static> MountableStorage for MemoryCardStorage<T> {
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

    fn finish(&self, path: &str) -> Result<(), Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        self.worker.shutdown()
    }
}

struct Handle {
    image: Arc<CardImage>,
    worker: ReconcileWorker,
    position: u64,
}

impl Read for Handle {
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

impl Write for Handle {
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
        self.worker.reconcile().map_err(std::io::Error::other)
    }
}

impl Seek for Handle {
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
