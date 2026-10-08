use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use dashmap::DashMap;
use tokio::sync::Mutex;

use crate::Error;

#[derive(Debug)]
pub(crate) struct StorageOperation {
    pub(crate) gate: Mutex<()>,
    pub(crate) pending: PathBuf,
    pub(crate) recovering: AtomicBool,
}

impl StorageOperation {
    pub(crate) fn open(repository: &Path, volume: uuid::Uuid) -> Result<Arc<Self>, Error> {
        static OPERATIONS: OnceLock<DashMap<PathBuf, Weak<StorageOperation>>> = OnceLock::new();
        let repository =
            fs::canonicalize(repository).map_err(|error| Error::io(repository, error))?;
        let pending = repository
            .join("polymer/operations")
            .join(volume.simple().to_string())
            .join("pending");
        let mut entry = OPERATIONS
            .get_or_init(DashMap::new)
            .entry(pending.clone())
            .or_default();
        if let Some(operation) = entry.upgrade() {
            return Ok(operation);
        }
        let recovering = pending
            .try_exists()
            .map_err(|error| Error::io(&pending, error))?;
        let operation = Arc::new(Self {
            gate: Mutex::new(()),
            pending,
            recovering: AtomicBool::new(recovering),
        });
        *entry = Arc::downgrade(&operation);
        Ok(operation)
    }

    pub(crate) fn ensure_ready(&self) -> Result<(), Error> {
        let recovering = self
            .pending
            .try_exists()
            .map_err(|error| Error::io(&self.pending, error))?;
        self.recovering.store(recovering, Ordering::Release);
        self.ensure_recovered()
    }

    pub(crate) fn ensure_recovered(&self) -> Result<(), Error> {
        if self.recovering.load(Ordering::Acquire) {
            return Err(Error::HistoryConflict(
                "profile operation requires recovery".to_owned(),
            ));
        }
        Ok(())
    }
}
