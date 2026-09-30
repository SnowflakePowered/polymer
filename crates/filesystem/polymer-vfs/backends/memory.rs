use crate::{
    AdoptHandle, FileKind, FileNode, Materialized, Metadata, MetadataChanges, NoopResourceLease,
    OpenFile, OpenOptions, ResourceFactory, VfsError, VfsResult,
};
use parking_lot::Mutex;
use polymer_consolestorage::{MemoryFile as StorageMemoryFile, StorageFile};
use std::sync::Arc;

pub struct MemoryFileFactory {
    file: StorageMemoryFile,
    writable: bool,
}

impl MemoryFileFactory {
    pub fn immutable(bytes: Vec<u8>) -> Self {
        Self {
            file: StorageMemoryFile::new(bytes),
            writable: false,
        }
    }

    pub fn writable(bytes: Vec<u8>) -> Self {
        Self {
            file: StorageMemoryFile::new(bytes),
            writable: true,
        }
    }
}

impl ResourceFactory<dyn FileNode> for MemoryFileFactory {
    fn materialize(&self) -> VfsResult<Materialized<dyn FileNode>> {
        Ok(Materialized {
            backend: Arc::new(MemoryFile {
                file: self.file.clone(),
                writable: self.writable,
            }),
            lease: Arc::new(NoopResourceLease),
        })
    }
}

struct MemoryFile {
    file: StorageMemoryFile,
    writable: bool,
}

impl FileNode for MemoryFile {
    fn metadata(&self) -> VfsResult<Metadata> {
        let metadata = self.file.metadata();
        Ok(Metadata {
            kind: FileKind::File,
            len: metadata.len,
            readonly: !self.writable,
            sparse: false,
        })
    }

    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()> {
        VfsError::require_writable(self.writable)?;
        if let Some(len) = changes.len {
            self.file
                .handle(true)
                .set_len(len)
                .map_err(|error| super::storage::storage_error(error, ""))?;
        }
        Ok(())
    }

    fn open(&self, options: OpenOptions) -> VfsResult<Box<dyn OpenFile>> {
        Ok(Box::new(MemoryOpen::new(
            &self.file,
            !self.writable,
            options,
        )?))
    }

    fn adopt(&self, source: Arc<dyn FileNode>) -> VfsResult<Arc<dyn AdoptHandle>> {
        VfsError::require_writable(self.writable)?;
        let adoption = Arc::new(MemoryFileAdoption {
            file: self.file.clone(),
            source,
            finalizing: Mutex::new(()),
        });
        adoption.finalize()?;
        Ok(adoption)
    }
}

struct MemoryFileAdoption {
    file: StorageMemoryFile,
    source: Arc<dyn FileNode>,
    finalizing: Mutex<()>,
}

impl AdoptHandle for MemoryFileAdoption {
    fn write(&self, offset: u64, buffer: &[u8]) -> VfsResult<()> {
        let _finalizing = self.finalizing.lock();
        self.file
            .handle(true)
            .write_all_at(offset, buffer)
            .map_err(|error| super::storage::storage_error(error, ""))
    }

    fn truncate(&self, len: u64) -> VfsResult<()> {
        let _finalizing = self.finalizing.lock();
        self.file
            .handle(true)
            .set_len(len)
            .map_err(|error| super::storage::storage_error(error, ""))
    }

    fn finalize(&self) -> VfsResult<()> {
        let _finalizing = self.finalizing.lock();
        let len = usize::try_from(self.source.metadata()?.len)
            .map_err(|_| VfsError::Unsupported("file length overflow".into()))?;
        let source = self.source.open(OpenOptions {
            read: true,
            write: false,
            truncate: false,
        })?;
        let mut replacement = vec![0_u8; len];
        let mut offset = 0;
        while offset < len {
            let read = source.read(offset as u64, &mut replacement[offset..])?;
            if read == 0 {
                return Err(VfsError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "adopted source became shorter while reconciling",
                )));
            }
            offset += read;
        }
        // Only publish a fully read replacement. Every open handle shares the contents.
        self.file.replace_contents(replacement);
        Ok(())
    }
}

struct MemoryOpen {
    handle: Arc<dyn StorageFile>,
    readonly: bool,
    write_access: bool,
}

impl MemoryOpen {
    fn new(file: &StorageMemoryFile, readonly: bool, options: OpenOptions) -> VfsResult<Self> {
        if options.write || options.truncate {
            VfsError::require_writable(!readonly)?;
        }
        if options.truncate {
            file.handle(true)
                .set_len(0)
                .map_err(|error| super::storage::storage_error(error, ""))?;
        }
        let write_access = !readonly && options.write;
        Ok(Self {
            handle: file.handle(write_access),
            readonly,
            write_access,
        })
    }
}

impl OpenFile for MemoryOpen {
    fn metadata(&self) -> VfsResult<Metadata> {
        let metadata = self
            .handle
            .metadata()
            .map_err(|error| super::storage::storage_error(error, ""))?;
        Ok(Metadata {
            kind: FileKind::File,
            len: metadata.len,
            readonly: self.readonly,
            sparse: false,
        })
    }

    fn read(&self, offset: u64, buffer: &mut [u8]) -> VfsResult<usize> {
        self.handle
            .read_at(offset, buffer)
            .map_err(|error| super::storage::storage_error(error, ""))
    }

    fn write(&self, offset: u64, buffer: &[u8]) -> VfsResult<usize> {
        VfsError::require_writable(self.write_access)?;
        self.handle
            .write_at(offset, buffer)
            .map_err(|error| super::storage::storage_error(error, ""))
    }

    fn truncate(&self, len: u64) -> VfsResult<()> {
        VfsError::require_writable(self.write_access)?;
        self.handle
            .set_len(len)
            .map_err(|error| super::storage::storage_error(error, ""))
    }
    fn flush(&self) -> VfsResult<()> {
        self.handle
            .flush()
            .map_err(|error| super::storage::storage_error(error, ""))
    }
    fn sync(&self) -> VfsResult<()> {
        self.handle
            .sync()
            .map_err(|error| super::storage::storage_error(error, ""))
    }
}
