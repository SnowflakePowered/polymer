use std::sync::Arc;

use jiff::Timestamp;
use parking_lot::Mutex;

use super::ReconcileWorker;
use crate::{EntryKind, Error, Metadata, StorageFile};

#[derive(Clone)]
pub(crate) struct MemoryFile(Arc<Mutex<MemoryFileState>>);

struct MemoryFileState {
    bytes: Vec<u8>,
    mtime: Timestamp,
}

impl MemoryFile {
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        Self(Arc::new(Mutex::new(MemoryFileState {
            bytes,
            mtime: Timestamp::now(),
        })))
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.0.lock().bytes.clone()
    }

    pub(crate) fn metadata(&self) -> Metadata {
        let state = self.0.lock();
        Metadata {
            kind: EntryKind::File,
            len: state.bytes.len() as u64,
            mtime: state.mtime,
        }
    }

    pub(crate) fn handle(
        &self,
        worker: ReconcileWorker,
        writable: bool,
        schedule_writeback: bool,
    ) -> Arc<dyn StorageFile> {
        Arc::new(MemoryStorageFile {
            file: self.clone(),
            worker,
            writable,
            schedule_writeback,
        })
    }
}

struct MemoryStorageFile {
    file: MemoryFile,
    worker: ReconcileWorker,
    writable: bool,
    schedule_writeback: bool,
}

impl MemoryStorageFile {
    fn require_writable(&self) -> Result<(), Error> {
        if self.writable {
            Ok(())
        } else {
            Err(Error::Unsupported(
                "in-memory file handle is read-only".to_owned(),
            ))
        }
    }

    fn schedule(&self) {
        if self.schedule_writeback {
            self.worker.schedule();
        }
    }
}

impl StorageFile for MemoryStorageFile {
    fn metadata(&self) -> Result<Metadata, Error> {
        Ok(self.file.metadata())
    }

    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, Error> {
        let state = self.file.0.lock();
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(state.bytes.len());
        let copied = output.len().min(state.bytes.len() - start);
        output[..copied].copy_from_slice(&state.bytes[start..start + copied]);
        Ok(copied)
    }

    fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, Error> {
        self.require_writable()?;
        let start = usize::try_from(offset)
            .map_err(|_| Error::Unsupported("in-memory file offset is too large".to_owned()))?;
        let end = start
            .checked_add(input.len())
            .ok_or_else(|| Error::Unsupported("in-memory file write is too large".to_owned()))?;
        let mut state = self.file.0.lock();
        if end > state.bytes.len() {
            state.bytes.resize(end, 0);
        }
        state.bytes[start..end].copy_from_slice(input);
        state.mtime = Timestamp::now();
        drop(state);
        self.schedule();
        Ok(input.len())
    }

    fn set_len(&self, len: u64) -> Result<(), Error> {
        self.require_writable()?;
        let len = usize::try_from(len)
            .map_err(|_| Error::Unsupported("in-memory file is too large".to_owned()))?;
        let mut state = self.file.0.lock();
        state.bytes.resize(len, 0);
        state.mtime = Timestamp::now();
        drop(state);
        self.schedule();
        Ok(())
    }

    fn flush(&self) -> Result<(), Error> {
        self.schedule();
        Ok(())
    }

    fn sync(&self) -> Result<(), Error> {
        self.worker.reconcile()
    }
}
