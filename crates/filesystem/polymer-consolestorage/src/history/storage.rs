use std::io::Read;

use super::SaveVersioning;
use crate::storage::VolumeOverlay;
use crate::{Error, GitVolumeStorage, Storage, VolumeStorage};

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

    pub(crate) fn hold_writes(&self) -> crate::storage::WritePass {
        self.storage.hold_writes()
    }

    pub(crate) fn write_if_changed(&self, path: &str, bytes: &[u8]) -> Result<(), Error> {
        if self
            .metadata(path)
            .is_ok_and(|metadata| metadata.len == bytes.len() as u64)
            && let Ok(mut existing) = self.open_read(path)
        {
            let mut current = Vec::with_capacity(bytes.len());
            if existing.read_to_end(&mut current).is_ok() && current == bytes {
                return Ok(());
            }
        }
        self.install_bytes(bytes, path)?;
        Ok(())
    }
}

impl VolumeOverlay for VersionedStorage {
    fn inner(&self) -> &dyn VolumeStorage {
        &self.storage
    }
}
