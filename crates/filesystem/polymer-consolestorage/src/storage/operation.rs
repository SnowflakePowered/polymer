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
    pub(crate) fn pending_marker(git_dir: &Path, volume: uuid::Uuid) -> PathBuf {
        git_dir
            .join("polymer/operations")
            .join(volume.simple().to_string())
            .join("pending")
    }

    pub(crate) fn journal_ref(volume: uuid::Uuid) -> String {
        format!("refs/consoles/transactions/v2/{}", volume.simple())
    }

    pub(crate) fn open(git_dir: &Path, volume: uuid::Uuid) -> Result<Arc<Self>, Error> {
        static OPERATIONS: OnceLock<DashMap<PathBuf, Weak<StorageOperation>>> = OnceLock::new();
        let pending = Self::pending_marker(git_dir, volume);
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
