use std::sync::Arc;

use polymer_consolestorage::{BatterySaveRepo, BatterySaveView};
use uuid::Uuid;

use crate::{
    DirectoryNode, ExclusiveResourceKey, FinalizationReason, MaterializationScope, Node,
    ResourceFactory, ResourceFinalizer, VfsError, VfsPath, VfsResult, Volume,
};

use super::storage::{StorageFileNode, unrestricted};

/// Exposes one battery-backed save as a format-free storage mount.
pub struct BatteryStorageFactory {
    repository: Arc<BatterySaveRepo>,
    resource_id: Uuid,
}

impl BatteryStorageFactory {
    pub fn new(repository: Arc<BatterySaveRepo>, resource_id: Uuid) -> Self {
        Self {
            repository,
            resource_id,
        }
    }
}

impl ResourceFactory<dyn Volume> for BatteryStorageFactory {
    fn resource_keys(&self) -> Vec<ExclusiveResourceKey> {
        vec![ExclusiveResourceKey::Resource(self.resource_id)]
    }

    fn materialize_in(&self, scope: &mut MaterializationScope) -> VfsResult<Arc<dyn Volume>> {
        let view = self.repository.mount()?;
        let volume = Arc::new(BatteryStorageVolume { view });
        scope.push(volume.clone());
        Ok(volume)
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
        Ok(Node::File(Arc::new(StorageFileNode::new(
            self.view.storage().clone(),
            self.view.entry().to_owned(),
            unrestricted(),
        ))))
    }

    fn rename(&self, _source: &VfsPath, _target: &VfsPath) -> VfsResult<()> {
        Err(VfsError::Unsupported(
            "a battery storage root cannot be renamed".to_owned(),
        ))
    }
}

impl ResourceFinalizer for BatteryStorageVolume {
    fn finalize(&self, reason: FinalizationReason) -> VfsResult<()> {
        if reason != FinalizationReason::LaunchFailed {
            self.view.storage().finish(self.view.entry())?;
        }
        Ok(())
    }
}
