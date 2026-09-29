use crate::{
    ExclusiveResourceKey, FileNode, FinalizationReason, Materialized, Metadata, OpenFile,
    ResourceFactory, ResourceLease, VfsResult, Volume,
};
use parking_lot::Mutex;
use std::{fs::File, path::PathBuf, sync::Arc};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;

use platform::{HostFile, HostVolume};

pub struct HostFileFactory {
    path: PathBuf,
    writable: bool,
}

impl HostFileFactory {
    pub fn new(path: PathBuf, writable: bool) -> Self {
        Self { path, writable }
    }
}

impl ResourceFactory<dyn FileNode> for HostFileFactory {
    fn materialize(&self) -> VfsResult<Materialized<dyn FileNode>> {
        let lease = ExclusiveResourceKey::HostPath(self.path.clone()).acquire()?;
        Ok(Materialized {
            backend: Arc::new(HostFile::new(&self.path, self.writable)?),
            lease,
        })
    }
}

pub struct PackageDirectoryFactory {
    root: PathBuf,
    internal_links: bool,
}

impl PackageDirectoryFactory {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            internal_links: false,
        }
    }

    /// Expose bundle aliases as ordinary nodes, confined to this read-only tree.
    pub fn with_internal_links(mut self) -> Self {
        self.internal_links = true;
        self
    }
}

impl ResourceFactory<dyn Volume> for PackageDirectoryFactory {
    fn materialize(&self) -> VfsResult<Materialized<dyn Volume>> {
        let lease = ExclusiveResourceKey::HostPath(self.root.clone()).acquire()?;
        Ok(Materialized {
            backend: Arc::new(HostVolume::new(&self.root, false, self.internal_links)?),
            lease,
        })
    }
}

/// A directory on the host filesystem exposed with explicit write authority.
///
/// Package directories use [`PackageDirectoryFactory`] and are always
/// read-only. This factory is for trusted host-owned storage such as persistent
/// plugin caches and temporary materialized data.
pub struct HostDirectoryFactory {
    root: PathBuf,
    writable: bool,
}

impl HostDirectoryFactory {
    pub fn new(root: PathBuf, writable: bool) -> Self {
        Self { root, writable }
    }
}

impl ResourceFactory<dyn Volume> for HostDirectoryFactory {
    fn materialize(&self) -> VfsResult<Materialized<dyn Volume>> {
        let lease = ExclusiveResourceKey::HostPath(self.root.clone()).acquire()?;
        Ok(Materialized {
            backend: Arc::new(HostVolume::new(&self.root, self.writable, false)?),
            lease,
        })
    }
}

pub struct TempDirectoryFactory {
    root: Option<PathBuf>,
}

impl TempDirectoryFactory {
    pub const fn new() -> Self {
        Self { root: None }
    }

    pub fn in_root(root: PathBuf) -> Self {
        Self { root: Some(root) }
    }
}

impl Default for TempDirectoryFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceFactory<dyn Volume> for TempDirectoryFactory {
    fn materialize(&self) -> VfsResult<Materialized<dyn Volume>> {
        let directory = match &self.root {
            Some(root) => {
                std::fs::create_dir_all(root)?;
                tempfile::Builder::new()
                    .prefix("polymer-scratch-")
                    .tempdir_in(root)?
            }
            None => tempfile::tempdir()?,
        };
        let backend = Arc::new(HostVolume::new(directory.path(), true, false)?);
        Ok(Materialized {
            backend,
            lease: Arc::new(TemporaryLease(Mutex::new(Some(directory)))),
        })
    }
}

struct TemporaryLease(Mutex<Option<tempfile::TempDir>>);

impl ResourceLease for TemporaryLease {
    fn finalize(&self, _reason: FinalizationReason) -> VfsResult<()> {
        self.0.lock().take();
        Ok(())
    }
}

struct HostOpen {
    file: File,
    writable: bool,
}

impl OpenFile for HostOpen {
    fn metadata(&self) -> VfsResult<Metadata> {
        platform::metadata_for(&self.file, self.writable)
    }

    fn read(&self, offset: u64, buffer: &mut [u8]) -> VfsResult<usize> {
        #[cfg(unix)]
        use std::os::unix::fs::FileExt;
        #[cfg(windows)]
        use std::os::windows::fs::FileExt;
        #[cfg(unix)]
        let read = self.file.read_at(buffer, offset)?;
        #[cfg(windows)]
        let read = self.file.seek_read(buffer, offset)?;
        Ok(read)
    }

    fn write(&self, offset: u64, buffer: &[u8]) -> VfsResult<usize> {
        #[cfg(unix)]
        use std::os::unix::fs::FileExt;
        #[cfg(windows)]
        use std::os::windows::fs::FileExt;
        #[cfg(unix)]
        let written = self.file.write_at(buffer, offset)?;
        #[cfg(windows)]
        let written = self.file.seek_write(buffer, offset)?;
        Ok(written)
    }

    fn truncate(&self, len: u64) -> VfsResult<()> {
        self.file.set_len(len)?;
        Ok(())
    }

    fn flush(&self) -> VfsResult<()> {
        Ok(())
    }

    fn sync(&self) -> VfsResult<()> {
        self.file.sync_all()?;
        Ok(())
    }
}
