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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CreateOptions, OpenOptions, VfsComponent};
    use snowglass_consolestorage::{DirectoryStorage, Storage};
    use std::io::Read;

    #[tokio::test]
    async fn file_factory_adapts_the_selected_view_entry() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(DirectoryStorage::new(directory.path()).await.unwrap());
        storage.create("cards/card.mcd").unwrap();
        let view = MemoryCardView::new(storage.clone(), "cards/card.mcd").unwrap();
        let materialized = MemoryCardFileFactory::new(view)
            .unwrap()
            .materialize()
            .unwrap();

        let file = materialized
            .backend
            .open(OpenOptions {
                read: true,
                write: true,
                truncate: false,
            })
            .unwrap();
        file.write_at(4, b"card").unwrap();
        file.sync().unwrap();

        let mut bytes = Vec::new();
        storage
            .open_read("cards/card.mcd")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"\0\0\0\0card");
    }

    #[tokio::test]
    async fn volume_factory_rebases_the_selected_directory() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(DirectoryStorage::new(directory.path()).await.unwrap());
        storage.create_dir_all("cards/gci").unwrap();
        let view = MemoryCardView::new(storage.clone(), "cards/gci").unwrap();
        let materialized = MemoryCardVolumeFactory::new(view)
            .unwrap()
            .materialize()
            .unwrap();
        let root = materialized.backend.root().unwrap();
        let name = VfsComponent::new("save.gci").unwrap();
        let file = root
            .create_file(&name, CreateOptions { exclusive: true })
            .unwrap();
        file.open(OpenOptions {
            read: false,
            write: true,
            truncate: false,
        })
        .unwrap()
        .write_at(0, b"gci")
        .unwrap();

        let source = VfsPathBuf::new("/save.gci").unwrap();
        let target = VfsPathBuf::new("/renamed.gci").unwrap();
        materialized
            .backend
            .rename(source.as_path(), target.as_path())
            .unwrap();

        assert!(!storage.exists("cards/gci/save.gci"));
        assert!(storage.exists("cards/gci/renamed.gci"));
    }

    #[tokio::test]
    async fn factories_reject_the_wrong_entry_kind() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(DirectoryStorage::new(directory.path()).await.unwrap());
        storage.create("card.raw").unwrap();

        let file = MemoryCardView::new(storage.clone(), "card.raw").unwrap();
        assert!(MemoryCardVolumeFactory::new(file).is_err());

        let directory = MemoryCardView::new(storage, "").unwrap();
        assert!(MemoryCardFileFactory::new(directory).is_err());
    }
}
