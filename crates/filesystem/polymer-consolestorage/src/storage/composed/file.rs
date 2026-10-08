use std::sync::Arc;

use parking_lot::RwLock;

use super::operation::ComposedOperation;
use crate::{Error, Metadata, StorageFile};

#[derive(Default)]
pub(super) struct ComposedFileBinding {
    pub(super) replacement: RwLock<Option<Arc<dyn StorageFile>>>,
}

pub(super) struct ComposedFile {
    pub(super) handle: Arc<dyn StorageFile>,
    pub(super) binding: Arc<ComposedFileBinding>,
    pub(super) writable: bool,
    pub(super) operation: Arc<ComposedOperation>,
}

impl ComposedFile {
    fn backing(&self) -> Arc<dyn StorageFile> {
        self.binding
            .replacement
            .read()
            .as_ref()
            .unwrap_or(&self.handle)
            .clone()
    }

    fn require_writable(&self) -> Result<(), Error> {
        if self.writable {
            Ok(())
        } else {
            Err(Error::io(
                std::path::Path::new(""),
                std::io::Error::new(std::io::ErrorKind::PermissionDenied, "read-only handle"),
            ))
        }
    }
}

impl StorageFile for ComposedFile {
    fn metadata(&self) -> Result<Metadata, Error> {
        let _operation = self.operation.read()?;
        self.backing().metadata()
    }

    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
        let _operation = self.operation.read()?;
        self.backing().read_at(offset, buffer)
    }

    fn write_at(&self, offset: u64, buffer: &[u8]) -> Result<usize, Error> {
        self.require_writable()?;
        let _operation = self.operation.read()?;
        self.backing().write_at(offset, buffer)
    }

    fn write_all_at(&self, offset: u64, buffer: &[u8]) -> Result<(), Error> {
        self.require_writable()?;
        let _operation = self.operation.read()?;
        self.backing().write_all_at(offset, buffer)
    }

    fn set_len(&self, len: u64) -> Result<(), Error> {
        self.require_writable()?;
        let _operation = self.operation.read()?;
        self.backing().set_len(len)
    }

    fn flush(&self) -> Result<(), Error> {
        let _operation = self.operation.read()?;
        self.backing().flush()
    }

    fn sync(&self) -> Result<(), Error> {
        let _operation = self.operation.read()?;
        self.backing().sync()
    }
}
