use std::sync::Arc;

use parking_lot::Mutex;
use snowglass_consolestorage::savegame::SaveDepotId;
use snowglass_consolestorage::{EntryKind, MemoryCard, MemoryCardView};

use crate::{
    FileNode, FinalizationReason, Materialized, Node, NoopResourceLease, ResourceFactory,
    ResourceLease, VfsError, VfsPath, VfsPathBuf, VfsResult, Volume, VolumeMount,
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

/// Exposes the formatted views of a memory card through the ordinary storage
/// volume capability used by plugin launches.
pub struct MemoryCardStorageFactory<M: ?Sized> {
    card: Arc<M>,
    primary: Vec<SaveDepotId>,
    capacity_blocks: u32,
}

impl<M: ?Sized> MemoryCardStorageFactory<M> {
    pub fn new(card: Arc<M>, primary: Vec<SaveDepotId>, capacity_blocks: u32) -> Self {
        Self {
            card,
            primary,
            capacity_blocks,
        }
    }
}

impl<M> ResourceFactory<dyn Volume> for MemoryCardStorageFactory<M>
where
    M: MemoryCard + ?Sized + 'static,
{
    fn materialize(&self) -> VfsResult<Materialized<dyn Volume>> {
        let volume = Arc::new(MemoryCardStorageVolume {
            card: self.card.clone(),
            primary: self.primary.clone(),
            capacity_blocks: self.capacity_blocks,
            views: Mutex::new(Vec::new()),
        });
        Ok(Materialized {
            backend: volume.clone(),
            lease: volume,
        })
    }
}

struct MemoryCardStorageVolume<M: ?Sized> {
    card: Arc<M>,
    primary: Vec<SaveDepotId>,
    capacity_blocks: u32,
    views: Mutex<Vec<MemoryCardView>>,
}

impl<M> Volume for MemoryCardStorageVolume<M>
where
    M: MemoryCard + ?Sized + 'static,
{
    fn root(&self) -> VfsResult<Arc<dyn crate::DirectoryNode>> {
        Err(VfsError::Unsupported(
            "memory-card storage requires a format".into(),
        ))
    }

    fn mount(&self, format: Option<&str>) -> VfsResult<VolumeMount> {
        let format = format
            .ok_or_else(|| VfsError::Unsupported("memory-card storage requires a format".into()))?;
        let view = self
            .card
            .view(&self.primary, self.capacity_blocks, format)
            .map_err(|error| VfsError::Unsupported(error.to_string()))?;
        let mounted = match view.kind() {
            EntryKind::File => VolumeMount::node(Node::File(
                materialize_file(
                    view.storage().clone(),
                    view.entry().to_owned(),
                    unrestricted(),
                    Arc::new(NoopResourceLease),
                )
                .backend,
            )),
            EntryKind::Directory => {
                let volume = materialize_volume(
                    view.storage().clone(),
                    view.entry().to_owned(),
                    unrestricted(),
                    Arc::new(NoopResourceLease),
                )
                .backend;
                VolumeMount::volume(volume)?
            }
        };
        self.views.lock().push(view);
        Ok(mounted)
    }

    fn rename(&self, _source: &VfsPath, _target: &VfsPath) -> VfsResult<()> {
        Err(VfsError::Unsupported(
            "a memory-card storage root cannot be mutated".into(),
        ))
    }
}

impl<M> ResourceLease for MemoryCardStorageVolume<M>
where
    M: MemoryCard + ?Sized,
{
    fn finalize(&self, reason: FinalizationReason) {
        if reason == FinalizationReason::LaunchFailed {
            return;
        }
        for view in self.views.lock().iter() {
            if view.kind() == EntryKind::File {
                let _ = view.storage().sync(view.entry());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snowglass_consolestorage::{
        BoxFuture, ChangeSignal, Error, InstallResult, Metadata, ReadDir, Storage, StorageFile,
        StorageRead,
    };
    use std::fmt;
    use std::io::Write;

    struct TestCard {
        storage: Arc<snowglass_consolestorage::DirectoryStorage>,
    }

    impl fmt::Debug for TestCard {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.debug_struct("TestCard").finish_non_exhaustive()
        }
    }

    impl Storage for TestCard {
        fn exists(&self, path: &str) -> bool {
            self.storage.exists(path)
        }

        fn metadata(&self, path: &str) -> Result<Metadata, Error> {
            self.storage.metadata(path)
        }

        fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
            self.storage.read_dir(path, recursive)
        }

        fn create_dir_all(&self, path: &str) -> Result<(), Error> {
            self.storage.create_dir_all(path)
        }

        fn create(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
            self.storage.create(path)
        }

        fn open(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
            self.storage.open(path)
        }

        fn open_read(&self, path: &str) -> Result<Box<dyn StorageRead>, Error> {
            self.storage.open_read(path)
        }

        fn append(&self, path: &str) -> Result<Box<dyn StorageFile>, Error> {
            self.storage.append(path)
        }

        fn remove(&self, path: &str) -> Result<(), Error> {
            self.storage.remove(path)
        }

        fn watch(&self, paths: &[&str]) -> Result<Option<Box<dyn ChangeSignal>>, Error> {
            self.storage.watch(paths)
        }

        fn label(&self) -> BoxFuture<'_, Result<Option<String>, Error>> {
            self.storage.label()
        }

        fn set_label<'a>(&'a self, label: Option<&'a str>) -> BoxFuture<'a, Result<(), Error>> {
            self.storage.set_label(label)
        }

        fn install_file<'a>(
            &'a self,
            source: Box<dyn tokio::io::AsyncRead + Send + Unpin>,
            dest: &str,
        ) -> BoxFuture<'a, Result<InstallResult, Error>> {
            self.storage.install_file(source, dest)
        }
    }

    impl MemoryCard for TestCard {
        fn block_size(&self) -> u64 {
            8192
        }

        fn max_blocks(&self) -> u32 {
            15
        }

        fn view(
            &self,
            _primary: &[SaveDepotId],
            capacity_blocks: u32,
            format: &str,
        ) -> Result<MemoryCardView, Error> {
            if capacity_blocks != self.max_blocks() || format != "raw" {
                return Err(Error::Unsupported(format!(
                    "unsupported test card view '{format}'"
                )));
            }
            MemoryCardView::new(self.storage.clone(), "card.mcd")
        }
    }

    #[tokio::test]
    async fn memory_card_storage_requires_format_and_mounts_raw_as_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Arc::new(
            snowglass_consolestorage::DirectoryStorage::new(directory.path())
                .await
                .unwrap(),
        );
        storage
            .create("card.mcd")
            .unwrap()
            .write_all(b"card")
            .unwrap();
        let card = Arc::new(TestCard { storage });

        let resource = || {
            Arc::new(crate::Resource::new(
                Arc::new(MemoryCardStorageFactory::new(card.clone(), Vec::new(), 15)),
                crate::Permissions::ALL,
                crate::ResourceOrigin::Other("test-card".into()),
            ))
        };

        let mut missing = crate::FilesystemPlan::default();
        missing.mount_storage(
            "/card.mcd".parse().unwrap(),
            resource(),
            crate::Permissions::ALL,
            None,
        );
        let missing = missing.seal().unwrap();
        assert!(
            matches!(missing.materialize(), Err(VfsError::Unsupported(reason)) if reason.contains("requires a format"))
        );

        let mut raw = crate::FilesystemPlan::default();
        raw.mount_storage(
            "/card.mcd".parse().unwrap(),
            resource(),
            crate::Permissions::ALL,
            Some("raw".into()),
        );
        let raw = raw.seal().unwrap();
        raw.materialize().unwrap();
        assert_eq!(
            raw.metadata(&"/card.mcd".parse().unwrap()).unwrap().kind,
            crate::FileKind::File
        );
    }
}
