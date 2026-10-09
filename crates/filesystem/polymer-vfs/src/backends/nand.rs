use std::sync::Arc;

use polymer_consolestorage::MountableStorage;
use polymer_consolestorage::nand::VirtualNand;
use uuid::Uuid;

use crate::{
    ExclusiveResourceKey, FinalizationReason, MaterializationScope, ResourceFactory,
    ResourceFinalizer, VfsResult, Volume,
};

use super::storage::{StoragePermissions, StorageVolume};

pub struct NandVolumeFactory<N: ?Sized> {
    nand: Arc<N>,
    storage: Arc<dyn MountableStorage>,
    resource_ids: Vec<Uuid>,
}

impl<N: ?Sized> NandVolumeFactory<N> {
    pub fn new(nand: Arc<N>, storage: Arc<dyn MountableStorage>, resource_ids: Vec<Uuid>) -> Self {
        Self {
            nand,
            storage,
            resource_ids,
        }
    }
}

impl<N> ResourceFactory<dyn Volume> for NandVolumeFactory<N>
where
    N: VirtualNand + ?Sized + 'static,
{
    fn resource_keys(&self) -> Vec<ExclusiveResourceKey> {
        self.resource_ids
            .iter()
            .copied()
            .map(ExclusiveResourceKey::Resource)
            .collect()
    }

    fn materialize_in(&self, scope: &mut MaterializationScope) -> VfsResult<Arc<dyn Volume>> {
        let storage = self.storage.clone();
        let nand = self.nand.clone();
        let permissions: StoragePermissions = Arc::new(move |path| nand.permissions(path));

        scope.push(Arc::new(NandStorageLease {
            storage: storage.clone(),
        }));
        Ok(Arc::new(StorageVolume::new(
            storage,
            String::new(),
            permissions,
        )))
    }
}

struct NandStorageLease {
    storage: Arc<dyn MountableStorage>,
}

impl ResourceFinalizer for NandStorageLease {
    fn finalize(&self, _reason: FinalizationReason) -> VfsResult<()> {
        Ok(self.storage.finish("")?)
    }
}
