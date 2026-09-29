use std::sync::Arc;

use polymer_consolestorage::{BatterySaveRepo, BatterySaveView};
use uuid::Uuid;

use crate::{
    CompositeResourceLease, DirectoryNode, FinalizationReason, Materialized, Node,
    NoopResourceLease, ResourceFactory, ResourceLease, VfsError, VfsPath, VfsResult, Volume,
};

use super::storage::{materialize_file, unrestricted};

/// Exposes one battery-backed save as a format-free storage mount.
pub struct BatteryStorageFactory {
    repository: Arc<BatterySaveRepo>,
    resource_id: Option<Uuid>,
}

impl BatteryStorageFactory {
    pub fn new(repository: Arc<BatterySaveRepo>, resource_id: Uuid) -> Self {
        Self {
            repository,
            resource_id: Some(resource_id),
        }
    }

    /// Creates an adapter whose caller holds the profile lease. This lets an
    /// outer save-history factory keep exclusivity through its final commit.
    pub fn with_managed_lease(repository: Arc<BatterySaveRepo>) -> Self {
        Self {
            repository,
            resource_id: None,
        }
    }
}

impl ResourceFactory<dyn Volume> for BatteryStorageFactory {
    fn materialize(&self) -> VfsResult<Materialized<dyn Volume>> {
        let ownership = CompositeResourceLease::exclusive(self.resource_id)?;
        let view = self.repository.mount()?;
        let volume = Arc::new(BatteryStorageVolume { view });
        Ok(Materialized {
            backend: volume.clone(),
            lease: Arc::new(ownership.with(volume)),
        })
    }
}

struct BatteryStorageVolume {
    view: BatterySaveView,
}

impl Volume for BatteryStorageVolume {
    fn root(&self) -> VfsResult<Arc<dyn DirectoryNode>> {
        Err(VfsError::Unsupported(
            "battery storage is a single file".to_owned(),
        ))
    }

    fn mount(&self, format: Option<&str>) -> VfsResult<Node> {
        if let Some(format) = format {
            return Err(VfsError::Unsupported(format!(
                "battery storage does not support the '{format}' format"
            )));
        }
        Ok(Node::File(
            materialize_file(
                self.view.storage().clone(),
                self.view.entry().to_owned(),
                unrestricted(),
                Arc::new(NoopResourceLease),
            )
            .backend,
        ))
    }

    fn rename(&self, _source: &VfsPath, _target: &VfsPath) -> VfsResult<()> {
        Err(VfsError::Unsupported(
            "a battery storage root cannot be renamed".to_owned(),
        ))
    }
}

impl ResourceLease for BatteryStorageVolume {
    fn finalize(&self, reason: FinalizationReason) -> VfsResult<()> {
        if reason != FinalizationReason::LaunchFailed {
            self.view.storage().finish(self.view.entry())?;
        }
        Ok(())
    }
}
