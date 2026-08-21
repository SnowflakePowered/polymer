use std::sync::Arc;

use snowglass_consolestorage::{EntryKind, MemoryCardView};

use crate::{
    FileNode, FinalizationReason, Materialized, ResourceFactory, ResourceLease, VfsError,
    VfsPathBuf, VfsResult, Volume,
};

use super::storage::{materialize_file, materialize_volume, unrestricted};

/// Adapts a file entry from a [`MemoryCardView`] into a VFS resource.
pub struct MemoryCardFileFactory {
    view: MemoryCardView,
}

impl MemoryCardFileFactory {
    pub fn new(view: MemoryCardView) -> VfsResult<Self> {
        if view.kind() != EntryKind::File {
            return Err(VfsError::IsDirectory(vfs_path(view.entry())));
        }
        Ok(Self { view })
    }
}

impl ResourceFactory<dyn FileNode> for MemoryCardFileFactory {
    fn materialize(&self) -> VfsResult<Materialized<dyn FileNode>> {
        Ok(materialize_file(
            self.view.storage().clone(),
            self.view.entry().to_owned(),
            unrestricted(),
            Arc::new(MemoryCardLease {
                view: self.view.clone(),
            }),
        ))
    }
}

/// Adapts a directory entry from a [`MemoryCardView`] into a VFS volume.
pub struct MemoryCardVolumeFactory {
    view: MemoryCardView,
}

impl MemoryCardVolumeFactory {
    pub fn new(view: MemoryCardView) -> VfsResult<Self> {
        if view.kind() != EntryKind::Directory {
            return Err(VfsError::NotDirectory(vfs_path(view.entry())));
        }
        Ok(Self { view })
    }
}

impl ResourceFactory<dyn Volume> for MemoryCardVolumeFactory {
    fn materialize(&self) -> VfsResult<Materialized<dyn Volume>> {
        Ok(materialize_volume(
            self.view.storage().clone(),
            self.view.entry().to_owned(),
            unrestricted(),
            Arc::new(MemoryCardLease {
                view: self.view.clone(),
            }),
        ))
    }
}

struct MemoryCardLease {
    view: MemoryCardView,
}

impl ResourceLease for MemoryCardLease {
    fn finalize(&self, reason: FinalizationReason) {
        if reason != FinalizationReason::LaunchFailed && self.view.kind() == EntryKind::File {
            let _ = self.view.storage().sync(self.view.entry());
        }
    }
}

fn vfs_path(path: &str) -> VfsPathBuf {
    VfsPathBuf::new(format!("/{path}")).unwrap_or_else(|_| VfsPathBuf::root())
}
