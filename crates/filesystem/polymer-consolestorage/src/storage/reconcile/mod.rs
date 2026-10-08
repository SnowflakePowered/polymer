mod file;
mod worker;

use std::{fmt, sync::Arc};

use super::{
    BoxFuture, ChangeSignal, Error, InstallResult, Metadata, MountableStorage, ReadDir, Storage,
    StorageFile,
};
use file::ReconcileFile;
use worker::ReconcileWorker;

/// Storage that can reconcile its current contents with its persistent backing.
pub trait ReconciliableStorage: Storage + 'static {
    fn reconcile(&self) -> Result<(), Error>;

    /// Whether writes, flushes, and truncations of this file schedule reconciliation.
    /// Structural changes still reconcile, and every handle can explicitly sync.
    fn filter(path: &str) -> bool;
}

/// Reconciles mutations after writes settle, or immediately on sync.
pub struct ReconcileStorage<T: ReconciliableStorage> {
    storage: Arc<T>,
    worker: ReconcileWorker,
}

impl<T: ReconciliableStorage> ReconcileStorage<T> {
    pub fn new(storage: T, name: &str) -> Result<Self, Error> {
        let storage = Arc::new(storage);
        let background = storage.clone();
        let worker = ReconcileWorker::spawn(name, move || background.reconcile())?;
        Ok(Self { storage, worker })
    }
}

impl<T: ReconciliableStorage> fmt::Debug for ReconcileStorage<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReconcileStorage")
            .field("storage", &self.storage)
            .finish_non_exhaustive()
    }
}

impl<T: ReconciliableStorage> Storage for ReconcileStorage<T> {
    fn claims(&self, path: &str) -> bool {
        self.storage.claims(path)
    }
    fn exists(&self, path: &str) -> bool {
        self.storage.exists(path)
    }
    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        self.storage.metadata(path)
    }
    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        self.storage.read_dir(path, recursive)
    }
    fn watch(&self) -> Result<Option<Box<dyn ChangeSignal>>, Error> {
        self.storage.watch()
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        let mutation = self.worker.begin(true)?;
        self.storage.create_dir_all(path)?;
        mutation.complete()?;
        Ok(())
    }

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        let mutation = self.worker.begin(true)?;
        let file = self.storage.create(path)?;
        mutation.complete()?;
        Ok(Arc::new(ReconcileFile {
            file,
            worker: self.worker.clone(),
            schedule_writeback: T::filter(path),
        }))
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        Ok(Arc::new(ReconcileFile {
            file: self.storage.open(path, write)?,
            worker: self.worker.clone(),
            schedule_writeback: T::filter(path),
        }))
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        let mutation = self.worker.begin(true)?;
        self.storage.remove(path)?;
        mutation.complete()?;
        Ok(())
    }

    fn install_file<'a>(
        &'a self,
        source: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        let dest = dest.to_owned();
        Box::pin(async move {
            let mutation = self.worker.begin(true)?;
            let result = self.storage.install_file(source, &dest).await?;
            mutation.complete()?;
            Ok(result)
        })
    }
}

impl<T: ReconciliableStorage + MountableStorage> MountableStorage for ReconcileStorage<T> {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        let mutation = self.worker.begin(true)?;
        self.storage.rename(from, to)?;
        mutation.complete()?;
        Ok(())
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        let mutation = self.worker.begin(T::filter(path))?;
        self.storage.truncate(path, len)?;
        mutation.complete()?;
        Ok(())
    }

    fn sync(&self, path: &str) -> Result<(), Error> {
        self.storage.sync(path)?;
        self.worker.reconcile()
    }

    fn finish(&self, path: &str) -> Result<(), Error> {
        self.worker.shutdown()?;
        self.storage.finish(path)
    }
}
