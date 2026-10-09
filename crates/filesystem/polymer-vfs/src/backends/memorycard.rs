use std::sync::Arc;

use parking_lot::Mutex;
use polymer_consolestorage::savegame::SaveDepotId;
use polymer_consolestorage::{EntryKind, MemoryCard, MemoryCardView};
use uuid::Uuid;

use crate::{
    ExclusiveResourceKey, FileNode, FinalizationReason, MaterializationScope, Node, PathStr,
    ResourceFactory, ResourceFinalizer, VfsComponent, VfsError, VfsPath, VfsPathBuf, VfsResult,
    Volume,
};

use super::storage::{StorageFileNode, StorageVolume, unrestricted};

/// Adapts a file entry from a [`MemoryCardView`] into a VFS resource.
pub struct MemoryCardFileFactory {
    view: MemoryCardView,
    resource_id: Uuid,
}

impl MemoryCardFileFactory {
    pub fn new(view: MemoryCardView, resource_id: Uuid) -> VfsResult<Self> {
        if view.kind() != EntryKind::File {
            return Err(VfsError::IsDirectory(VfsPathBuf::storage(view.entry())));
        }
        Ok(Self { view, resource_id })
    }
}

impl ResourceFactory<dyn FileNode> for MemoryCardFileFactory {
    fn resource_keys(&self) -> Vec<ExclusiveResourceKey> {
        vec![ExclusiveResourceKey::Resource(self.resource_id)]
    }

    fn materialize_in(&self, scope: &mut MaterializationScope) -> VfsResult<Arc<dyn FileNode>> {
        scope.push(Arc::new(MemoryCardLease {
            view: self.view.clone(),
        }));
        Ok(Arc::new(StorageFileNode::new(
            self.view.storage().clone(),
            self.view.entry().to_owned(),
            unrestricted(),
        )))
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
            return Err(VfsError::NotDirectory(VfsPathBuf::storage(view.entry())));
        }
        Ok(Self { view, resource_id })
    }
}

impl ResourceFactory<dyn Volume> for MemoryCardVolumeFactory {
    fn resource_keys(&self) -> Vec<ExclusiveResourceKey> {
        vec![ExclusiveResourceKey::Resource(self.resource_id)]
    }

    fn materialize_in(&self, scope: &mut MaterializationScope) -> VfsResult<Arc<dyn Volume>> {
        scope.push(Arc::new(MemoryCardLease {
            view: self.view.clone(),
        }));
        Ok(Arc::new(StorageVolume::new(
            self.view.storage().clone(),
            self.view.entry().to_owned(),
            unrestricted(),
        )))
    }
}

struct MemoryCardLease {
    view: MemoryCardView,
}

impl ResourceFinalizer for MemoryCardLease {
    fn finalize(&self, reason: FinalizationReason) -> VfsResult<()> {
        if reason != FinalizationReason::LaunchFailed {
            self.view.storage().finish(self.view.entry())?;
        }
        Ok(())
    }
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
    fn resource_keys(&self) -> Vec<ExclusiveResourceKey> {
        vec![ExclusiveResourceKey::Resource(self.resource_id)]
    }

    fn materialize_in(&self, scope: &mut MaterializationScope) -> VfsResult<Arc<dyn Volume>> {
        let volume = Arc::new(MemoryCardStorageVolume {
            card: self.card.clone(),
            primary: self.primary.clone(),
            view: Mutex::new(None),
        });
        scope.push(volume.clone());
        Ok(volume)
    }
}

struct MemoryCardStorageVolume<M: ?Sized> {
    card: Arc<M>,
    primary: Vec<SaveDepotId>,
    view: Mutex<Option<MountedMemoryCardView>>,
}

struct MountedMemoryCardView {
    format: String,
    view: MemoryCardView,
    volume: Option<Arc<dyn Volume>>,
}

impl<M> Volume for MemoryCardStorageVolume<M>
where
    M: MemoryCard + ?Sized + 'static,
{
    fn root(&self) -> VfsResult<Arc<dyn crate::DirectoryNode>> {
        Err(VfsError::Unsupported(
            "memory-card storage has no directory root".into(),
        ))
    }

    fn mount(&self, format: Option<&str>) -> VfsResult<Node> {
        let format = format.unwrap_or("raw");
        let mut mounted = self.view.lock();
        if let Some(existing) = mounted.as_ref() {
            return Err(VfsError::Unsupported(format!(
                "the memory card is already mounted as '{}'",
                existing.format
            )));
        }
        let view = self.card.view(&self.primary, format)?;
        let (node, volume) = match view.kind() {
            EntryKind::File => (
                Node::File(Arc::new(StorageFileNode::new(
                    view.storage().clone(),
                    view.entry().to_owned(),
                    unrestricted(),
                ))),
                None,
            ),
            EntryKind::Directory => {
                let volume: Arc<dyn Volume> = Arc::new(StorageVolume::new(
                    view.storage().clone(),
                    view.entry().to_owned(),
                    unrestricted(),
                ));
                (Node::Directory(volume.root()?), Some(volume))
            }
        };
        *mounted = Some(MountedMemoryCardView {
            format: format.to_owned(),
            view,
            volume,
        });
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
            .view
            .lock()
            .as_ref()
            .filter(|mounted| mounted.format == format.as_ref() as &str)
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

impl<M> ResourceFinalizer for MemoryCardStorageVolume<M>
where
    M: MemoryCard + ?Sized,
{
    fn finalize(&self, reason: FinalizationReason) -> VfsResult<()> {
        if reason == FinalizationReason::LaunchFailed {
            return Ok(());
        }
        let view = self
            .view
            .lock()
            .as_ref()
            .map(|mounted| mounted.view.clone());
        match view {
            Some(view) => Ok(view.storage().finish(view.entry())?),
            None => Ok(()),
        }
    }
}
