use crate::{
    FileNode, FinalizationReason, Materialized, ResourceFactory, ResourceLease, VfsResult, Volume,
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
        Ok(Materialized {
            backend: Arc::new(HostFile::new(&self.path, self.writable)?),
            lease: Arc::new(crate::NoopResourceLease),
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
        Ok(Materialized {
            backend: Arc::new(HostVolume::new(&self.root, false)?),
            lease: Arc::new(crate::NoopResourceLease),
        })
    }
}

pub(super) fn temporary_volume() -> VfsResult<Materialized<dyn Volume>> {
    let directory = tempfile::tempdir()?;
    let backend = Arc::new(HostVolume::new(directory.path(), true)?);
    Ok(Materialized {
        backend,
        lease: Arc::new(TemporaryLease(Mutex::new(Some(directory)))),
    })
}

struct TemporaryLease(Mutex<Option<tempfile::TempDir>>);

impl ResourceLease for TemporaryLease {
    fn finalize(&self, _reason: FinalizationReason) {
        self.0.lock().take();
    }
}
