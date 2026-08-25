#[cfg(test)]
use uuid::Uuid;

use super::SaveVersioning;
use crate::storage::VolumeOverlay;
use crate::{Error, GitVolumeStorage, VolumeStorage};

/// A Git profile volume together with its independently addressable save histories.
#[derive(Clone, Debug)]
pub struct VersionedStorage {
    storage: GitVolumeStorage,
    history: SaveVersioning,
}

impl VersionedStorage {
    pub fn new(storage: GitVolumeStorage) -> Result<Self, Error> {
        let history = SaveVersioning::open_platform(storage.clone())?;
        Ok(Self { storage, history })
    }

    pub fn versioning(&self) -> &SaveVersioning {
        &self.history
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        root: &std::path::Path,
        platform: &str,
        id: Uuid,
        kind: crate::StorageKind,
    ) -> Self {
        let repository = crate::PlatformRepository::open(root.join("platform"), platform).unwrap();
        Self::new(repository.volume(kind, id)).unwrap()
    }
}

impl VolumeOverlay for VersionedStorage {
    fn inner(&self) -> &dyn VolumeStorage {
        &self.storage
    }
}
