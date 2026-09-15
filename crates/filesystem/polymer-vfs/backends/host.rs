use crate::{
    ExclusiveResourceKey, FileNode, FinalizationReason, Materialized, ResourceFactory,
    ResourceLease, VfsResult, Volume,
};
use parking_lot::Mutex;
use std::{path::PathBuf, sync::Arc};

#[cfg(unix)]
#[path = "host/unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "host/windows.rs"]
mod platform;

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
}

impl PackageDirectoryFactory {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl ResourceFactory<dyn Volume> for PackageDirectoryFactory {
    fn materialize(&self) -> VfsResult<Materialized<dyn Volume>> {
        let lease = ExclusiveResourceKey::HostPath(self.root.clone()).acquire()?;
        Ok(Materialized {
            backend: Arc::new(HostVolume::new(&self.root, false)?),
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
            backend: Arc::new(HostVolume::new(&self.root, self.writable)?),
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
        let backend = Arc::new(HostVolume::new(directory.path(), true)?);
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
