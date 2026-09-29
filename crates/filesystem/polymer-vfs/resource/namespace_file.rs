use crate::{AdoptHandle, FileNode, Metadata, MetadataChanges, OpenFile, OpenOptions, VfsResult};
use parking_lot::RwLock;
use std::sync::Arc;

pub(super) struct NamespaceFile {
    backend: Arc<dyn FileNode>,
    adoption: Arc<RwLock<Option<Adoption>>>,
}

#[derive(Clone)]
pub(super) struct Adoption {
    target: Arc<dyn FileNode>,
    handle: Arc<dyn AdoptHandle>,
}

impl NamespaceFile {
    pub(super) fn new(backend: Arc<dyn FileNode>) -> Self {
        Self {
            backend,
            adoption: Arc::new(RwLock::new(None)),
        }
    }

    pub(super) fn prepare_adoption(&self, displaced: &NamespaceFile) -> VfsResult<Adoption> {
        let target = displaced
            .adoption
            .read()
            .as_ref()
            .map(|adoption| adoption.target.clone())
            .unwrap_or_else(|| displaced.backend.clone());
        let handle = target.adopt(self.backend.clone())?;
        Ok(Adoption { target, handle })
    }

    pub(super) fn install_adoption(&self, adoption: Adoption, displaced: &NamespaceFile) {
        *self.adoption.write() = Some(adoption);
        displaced.adoption.write().take();
    }

    pub(super) fn finalize_adoption(&self) -> VfsResult<()> {
        match self.adoption.read().clone() {
            Some(adoption) => adoption.handle.finalize(),
            None => Ok(()),
        }
    }
}

impl FileNode for NamespaceFile {
    fn metadata(&self) -> VfsResult<Metadata> {
        self.backend.metadata()
    }

    fn set_metadata(&self, changes: MetadataChanges) -> VfsResult<()> {
        self.backend.set_metadata(changes)?;
        self.finalize_adoption()
    }

    fn open(&self, options: OpenOptions) -> VfsResult<Box<dyn OpenFile>> {
        Ok(Box::new(NamespaceOpenFile {
            inner: self.backend.open(options)?,
            adoption: self.adoption.clone(),
        }))
    }
}

struct NamespaceOpenFile {
    inner: Box<dyn OpenFile>,
    adoption: Arc<RwLock<Option<Adoption>>>,
}

impl NamespaceOpenFile {
    fn adoption(&self) -> Option<Adoption> {
        self.adoption.read().clone()
    }

    fn finalize_adoption(&self) -> VfsResult<()> {
        match self.adoption() {
            Some(adoption) => adoption.handle.finalize(),
            None => Ok(()),
        }
    }
}

impl OpenFile for NamespaceOpenFile {
    fn metadata(&self) -> VfsResult<Metadata> {
        self.inner.metadata()
    }

    fn read(&self, offset: u64, buffer: &mut [u8]) -> VfsResult<usize> {
        self.inner.read(offset, buffer)
    }

    fn write(&self, offset: u64, buffer: &[u8]) -> VfsResult<usize> {
        let written = self.inner.write(offset, buffer)?;
        if let Some(adoption) = self.adoption() {
            adoption.handle.write(offset, &buffer[..written])?;
        }
        Ok(written)
    }

    fn truncate(&self, len: u64) -> VfsResult<()> {
        self.inner.truncate(len)?;
        if let Some(adoption) = self.adoption() {
            adoption.handle.truncate(len)?;
        }
        Ok(())
    }

    fn flush(&self) -> VfsResult<()> {
        self.inner.flush()?;
        self.finalize_adoption()
    }

    fn sync(&self) -> VfsResult<()> {
        self.inner.sync()?;
        self.finalize_adoption()
    }
}
