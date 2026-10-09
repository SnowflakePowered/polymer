use std::ops::Deref;
use std::sync::Arc;

use super::journal::Endpoint;
use crate::storage::operation::StorageOperation;
use crate::{Error, GitVolumeStorage, MountableStorage, SaveRepo, SaveVersioning};

#[derive(Debug)]
pub(crate) struct ComposedLayer {
    pub(super) id: String,
    storage: Arc<dyn MountableStorage>,
    persistence: Persistence,
}

#[derive(Debug)]
enum Persistence {
    Volatile,
    GitProfile {
        volume: GitVolumeStorage,
        history: SaveVersioning,
    },
}

impl ComposedLayer {
    pub(crate) fn volatile(id: &str, storage: Arc<dyn MountableStorage>) -> Self {
        Self {
            id: id.to_owned(),
            storage,
            persistence: Persistence::Volatile,
        }
    }

    pub(crate) fn saves(id: &str, storage: Arc<dyn SaveRepo>) -> Self {
        let history = storage.versioning().clone();
        let volume = history.profile_storage();
        Self {
            id: id.to_owned(),
            storage,
            persistence: Persistence::GitProfile { volume, history },
        }
    }

    pub(super) fn persistent(&self) -> bool {
        matches!(self.persistence, Persistence::GitProfile { .. })
    }

    pub(super) fn recovery_domain(&self) -> Option<(Arc<StorageOperation>, uuid::Uuid)> {
        match &self.persistence {
            Persistence::GitProfile { history, volume } => {
                Some((history.profile_operation(), volume.id()))
            }
            _ => None,
        }
    }

    pub(super) fn endpoint(&self, path: &str) -> Result<Endpoint, Error> {
        let endpoint = Endpoint {
            layer: self.id.clone(),
            path: path.to_owned(),
            volatile: !self.persistent(),
        };
        endpoint.validate()?;
        Ok(endpoint)
    }

    pub(super) fn acquire(&self) -> Result<Option<tokio::sync::MutexGuard<'_, ()>>, Error> {
        match &self.persistence {
            Persistence::GitProfile { history, .. } => history.try_profile_operation().map(Some),
            _ => Ok(None),
        }
    }

    pub(super) fn persist_changes(&self) -> Result<(), Error> {
        match &self.persistence {
            Persistence::GitProfile { volume, .. } => {
                volume.commit_pending("Polymer filesystem update")?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

impl Deref for ComposedLayer {
    type Target = Arc<dyn MountableStorage>;

    fn deref(&self) -> &Self::Target {
        &self.storage
    }
}
