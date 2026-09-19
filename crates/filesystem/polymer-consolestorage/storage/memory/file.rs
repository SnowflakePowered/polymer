use std::sync::Arc;

use jiff::Timestamp;
use parking_lot::Mutex;

use crate::{EntryKind, Error, Metadata, StorageFile};

/// Shared in-memory file contents, independent of a storage tree.
#[derive(Clone)]
pub struct MemoryFile(Arc<Mutex<MemoryFileState>>);

struct MemoryFileState {
    bytes: Vec<u8>,
    mtime: Timestamp,
}

impl MemoryFile {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Arc::new(Mutex::new(MemoryFileState {
            bytes,
            mtime: Timestamp::now(),
        })))
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.0.lock().bytes.clone()
    }

    pub fn metadata(&self) -> Metadata {
        let state = self.0.lock();
        Metadata {
            kind: EntryKind::File,
            len: state.bytes.len() as u64,
            mtime: state.mtime,
        }
    }

    /// Atomically replaces contents for all existing handles.
    pub fn replace_contents(&self, bytes: Vec<u8>) {
        let mut state = self.0.lock();
        state.bytes = bytes;
        state.mtime = Timestamp::now();
    }

    pub fn handle(&self, writable: bool) -> Arc<dyn StorageFile> {
        Arc::new(MemoryStorageFile {
            file: self.clone(),
            writable,
        })
    }
}

struct MemoryStorageFile {
    file: MemoryFile,
    writable: bool,
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
        Ok(())
    }

    fn flush(&self) -> Result<(), Error> {
        Ok(())
    }

    fn sync(&self) -> Result<(), Error> {
        Ok(())
    }
}
