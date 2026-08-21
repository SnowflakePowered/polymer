use std::sync::Arc;

use snowglass_consolestorage::Permissions as NandPermissions;
use snowglass_consolestorage::nand::VirtualNand;

use crate::{Materialized, NoopResourceLease, Permissions, ResourceFactory, VfsResult, Volume};

use super::storage::{StoragePermissions, materialize_volume};

impl From<NandPermissions> for Permissions {
    fn from(grants: NandPermissions) -> Self {
        Self::from_bits_truncate(grants.bits())
    }
}

pub struct NandVolumeFactory<N: ?Sized> {
    nand: Arc<N>,
}

impl<N: ?Sized> NandVolumeFactory<N> {
    pub fn new(nand: Arc<N>) -> Self {
        Self { nand }
    }
}

impl<N> ResourceFactory<dyn Volume> for NandVolumeFactory<N>
where
    N: VirtualNand + ?Sized + 'static,
{
    fn materialize(&self) -> VfsResult<Materialized<dyn Volume>> {
        let storage = self.nand.root();
        let nand = self.nand.clone();
        let permissions: StoragePermissions = Arc::new(move |path| nand.permissions(path).into());

        Ok(materialize_volume(
            storage,
            String::new(),
            permissions,
            Arc::new(NoopResourceLease),
        ))
    }
}
