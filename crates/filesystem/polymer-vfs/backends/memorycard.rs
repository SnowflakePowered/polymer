use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;
use polymer_consolestorage::savegame::SaveDepotId;
use polymer_consolestorage::{EntryKind, MemoryCard, MemoryCardView};
use uuid::Uuid;

use crate::{
    CompositeResourceLease, ExclusiveResourceKey, FileNode, FinalizationReason, Materialized, Node,
    NoopResourceLease, PathStr, ResourceFactory, ResourceLease, VfsComponent, VfsError, VfsPath,
    VfsPathBuf, VfsResult, Volume,
};

use super::storage::{materialize_file, materialize_volume, unrestricted};

/// Adapts a file entry from a [`MemoryCardView`] into a VFS resource.
pub struct MemoryCardFileFactory {
    view: MemoryCardView,
    resource_id: Uuid,
}

impl MemoryCardFileFactory {
    pub fn new(view: MemoryCardView, resource_id: Uuid) -> VfsResult<Self> {
        if view.kind() != EntryKind::File {
            return Err(VfsError::IsDirectory(vfs_path(view.entry())));
        }
        Ok(Self { view, resource_id })
    }
}

impl ResourceFactory<dyn FileNode> for MemoryCardFileFactory {
    fn materialize(&self) -> VfsResult<Materialized<dyn FileNode>> {
        let ownership = ExclusiveResourceKey::Resource(self.resource_id).acquire()?;
        let lease = CompositeResourceLease::new(vec![
            ownership,
            Arc::new(MemoryCardLease {
                view: self.view.clone(),
            }),
        ]);
        Ok(materialize_file(
            self.view.storage().clone(),
            self.view.entry().to_owned(),
            unrestricted(),
            lease,
        ))
    }
}

/// Adapts a directory entry from a [`MemoryCardView`] into a VFS volume.
pub struct MemoryCardVolumeFactory {
    view: MemoryCardView,
    resource_id: Uuid,
}

impl MemoryCardVolumeFactory {
    pub fn new(view: MemoryCardView, resource_id: Uuid) -> VfsResult<Self> {
        if view.kind() != EntryKind::Directory {
            return Err(VfsError::NotDirectory(vfs_path(view.entry())));
        }
        Ok(Self { view, resource_id })
    }
}

impl ResourceFactory<dyn Volume> for MemoryCardVolumeFactory {
    fn materialize(&self) -> VfsResult<Materialized<dyn Volume>> {
        let ownership = ExclusiveResourceKey::Resource(self.resource_id).acquire()?;
        let lease = CompositeResourceLease::new(vec![
            ownership,
            Arc::new(MemoryCardLease {
                view: self.view.clone(),
            }),
        ]);
        Ok(materialize_volume(
            self.view.storage().clone(),
            self.view.entry().to_owned(),
            unrestricted(),
            lease,
        ))
    }
}

struct MemoryCardLease {
    view: MemoryCardView,
}

impl ResourceLease for MemoryCardLease {
    fn finalize(&self, reason: FinalizationReason) -> VfsResult<()> {
        if reason != FinalizationReason::LaunchFailed {
            self.view
                .storage()
                .finish(self.view.entry())
                .map_err(console_storage_error)?;
        }
        Ok(())
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
    resource_id: Uuid,
}

impl<M: ?Sized> MemoryCardStorageFactory<M> {
    pub fn new(card: Arc<M>, primary: Vec<SaveDepotId>, resource_id: Uuid) -> Self {
        Self {
            card,
            primary,
            resource_id,
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
            views: Mutex::new(HashMap::new()),
        });
        let ownership = ExclusiveResourceKey::Resource(self.resource_id).acquire()?;
        let lease = CompositeResourceLease::new(vec![ownership, volume.clone()]);
        Ok(Materialized {
            backend: volume.clone(),
            lease,
        })
    }
}

struct MemoryCardStorageVolume<M: ?Sized> {
    card: Arc<M>,
    primary: Vec<SaveDepotId>,
    views: Mutex<HashMap<String, MountedMemoryCardView>>,
}

struct MountedMemoryCardView {
    view: MemoryCardView,
    node: Node,
    volume: Option<Arc<dyn Volume>>,
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

    fn mount(&self, format: Option<&str>) -> VfsResult<Node> {
        let format = format
            .ok_or_else(|| VfsError::Unsupported("memory-card storage requires a format".into()))?;
        let mut views = self.views.lock();
        if let Some(mounted) = views.get(format) {
            return Ok(mounted.node.clone());
        }
        let view = self
            .card
            .view(&self.primary, format)
            .map_err(|error| VfsError::Unsupported(error.to_string()))?;
        let (node, volume) = match view.kind() {
            EntryKind::File => (
                Node::File(
                    materialize_file(
                        view.storage().clone(),
                        view.entry().to_owned(),
                        unrestricted(),
                        Arc::new(NoopResourceLease),
                    )
                    .backend,
                ),
                None,
            ),
            EntryKind::Directory => {
                let volume = materialize_volume(
                    view.storage().clone(),
                    view.entry().to_owned(),
                    unrestricted(),
                    Arc::new(NoopResourceLease),
                )
                .backend;
                (Node::Directory(volume.root()?), Some(volume))
            }
        };
        views.insert(
            format.to_owned(),
            MountedMemoryCardView {
                view,
                node: node.clone(),
                volume,
            },
        );
        Ok(node)
    }

    fn rename(&self, source: &VfsPath, target: &VfsPath) -> VfsResult<()> {
        let (source_volume, source) = self.route(source)?;
        let (target_volume, target) = self.route(target)?;
        if !Arc::ptr_eq(&source_volume, &target_volume) {
            return Err(VfsError::CrossDevice);
        }
        source_volume.rename(source.as_path(), target.as_path())
    }

    fn replace(&self, source: &VfsPath, target: &VfsPath) -> VfsResult<()> {
        let (source_volume, source) = self.route(source)?;
        let (target_volume, target) = self.route(target)?;
        if !Arc::ptr_eq(&source_volume, &target_volume) {
            return Err(VfsError::CrossDevice);
        }
        source_volume.replace(source.as_path(), target.as_path())
    }

    fn detach(&self, source: &VfsPath) -> VfsResult<()> {
        let (volume, source) = self.route(source)?;
        volume.detach(source.as_path())
    }
}

impl<M: ?Sized> MemoryCardStorageVolume<M> {
    fn route(&self, path: &VfsPath) -> VfsResult<(Arc<dyn Volume>, VfsPathBuf)> {
        let mut components = path.components();
        let format = components.next().ok_or_else(|| {
            VfsError::Unsupported("a memory-card storage root cannot be mutated".into())
        })?;
        #[cfg(windows)]
        let format = format.to_string_lossy();
        #[cfg(unix)]
        let format = bstr::ByteSlice::to_str_lossy(format.as_ref() as &[u8]);
        let volume = self
            .views
            .lock()
            .get(format.as_ref() as &str)
            .and_then(|mounted| mounted.volume.clone())
            .ok_or_else(|| VfsError::NotDirectory(path.to_owned()))?;
        let mut relative = VfsPathBuf::root();
        for component in components {
            let component = VfsComponent::from_units(PathStr::as_slice(component).to_vec())?;
            relative = relative.as_path().join(&component);
        }
        Ok((volume, relative))
    }
}

impl<M> ResourceLease for MemoryCardStorageVolume<M>
where
    M: MemoryCard + ?Sized,
{
    fn finalize(&self, reason: FinalizationReason) -> VfsResult<()> {
        if reason == FinalizationReason::LaunchFailed {
            return Ok(());
        }
        let views = self
            .views
            .lock()
            .values()
            .map(|mounted| mounted.view.clone())
            .collect::<Vec<_>>();
        for view in views {
            view.storage()
                .finish(view.entry())
                .map_err(console_storage_error)?;
        }
        Ok(())
    }
}

fn console_storage_error(error: polymer_consolestorage::Error) -> VfsError {
    match error {
        polymer_consolestorage::Error::Io { source, .. } => VfsError::Io(source),
        error => VfsError::Unsupported(error.to_string()),
    }
}
