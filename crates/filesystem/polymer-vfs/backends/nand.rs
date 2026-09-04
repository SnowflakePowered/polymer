use std::sync::Arc;

use polymer_consolestorage::MountableStorage;
use polymer_consolestorage::Permissions as NandPermissions;
use polymer_consolestorage::nand::VirtualNand;
use uuid::Uuid;

use crate::{
    CompositeResourceLease, ExclusiveResourceKey, FinalizationReason, Materialized, Permissions,
    ResourceFactory, ResourceLease, VfsError, VfsResult, Volume,
};

use super::storage::{StoragePermissions, materialize_volume};

impl From<NandPermissions> for Permissions {
    fn from(grants: NandPermissions) -> Self {
        Self::from_bits_truncate(grants.bits())
    }
}

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
    fn materialize(&self) -> VfsResult<Materialized<dyn Volume>> {
        let storage = self.storage.clone();
        let nand = self.nand.clone();
        let permissions: StoragePermissions = Arc::new(move |path| nand.permissions(path).into());

        let mut leases = self
            .resource_ids
            .iter()
            .map(|id| ExclusiveResourceKey::Resource(*id).acquire())
            .collect::<VfsResult<Vec<_>>>()?;
        leases.push(Arc::new(NandStorageLease {
            storage: storage.clone(),
        }));
        let lease = CompositeResourceLease::new(leases);
        Ok(materialize_volume(
            storage,
            String::new(),
            permissions,
            lease,
        ))
    }
}

struct NandStorageLease {
    storage: Arc<dyn MountableStorage>,
}

impl ResourceLease for NandStorageLease {
    fn finalize(&self, _reason: FinalizationReason) -> VfsResult<()> {
        self.storage.finish("").map_err(|error| match error {
            polymer_consolestorage::Error::Io { source, .. } => VfsError::Io(source),
            error => VfsError::Unsupported(error.to_string()),
        })
    }
}
