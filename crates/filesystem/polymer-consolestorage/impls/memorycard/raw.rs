use std::fmt;
use std::sync::Arc;

use super::{MemoryFile, ReconcileWorker};
use crate::{
    BoxFuture, Error, InstallResult, MemoryCard, Metadata, MountableStorage, ReadDir, Reconcile,
    Storage, StorageFile,
};

pub(crate) struct CardImage {
    file: MemoryFile,
    reconcile: Reconcile,
}

impl CardImage {
    fn reconcile(&self) -> Result<(), Error> {
        let bytes = self.file.bytes();
        (self.reconcile)(&bytes)
    }
}

pub(crate) struct MemoryCardStorage<T: MemoryCard + 'static> {
    _backing: T,
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
            file: MemoryFile::new(bytes),
            reconcile,
        });
        let background = image.clone();
        let worker =
            ReconcileWorker::spawn(thread_name, display_name, move || background.reconcile())?;
        Ok(Self {
            _backing: card,
            image,
            worker,
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
        Ok(self.image.file.metadata())
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

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        let handle = self.image.file.handle(self.worker.clone(), true, true);
        handle.set_len(0)?;
        Ok(handle)
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        Ok(self.image.file.handle(self.worker.clone(), write, true))
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        if !path.is_empty() {
            return Err(Error::not_found(path));
        }
        self.image
            .file
            .handle(self.worker.clone(), true, true)
            .set_len(0)
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
        self.image
            .file
            .handle(self.worker.clone(), true, true)
            .set_len(len)
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
