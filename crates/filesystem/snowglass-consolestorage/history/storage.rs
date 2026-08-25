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
}

impl VolumeOverlay for VersionedStorage {
    fn inner(&self) -> &dyn VolumeStorage {
        &self.storage
    }
}
