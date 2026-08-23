use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::installable::artifacts::Artifact;
use crate::storage::VolumeStorage;
use crate::storage::manifest::{ArtifactSummary, VolumeInfo};
use crate::{Error, MountableStorage, Permissions, SaveRepo};

pub enum NandCompletion {
    /// The NAND is complete as far as minimally required to run a game.
    Minimal,
    /// The NAND is complete
    Full,
    /// The NAND is verified against an mtree
    Verified,
}

#[async_trait]
pub trait VirtualNand: Send + Sync {
    /// Lay out an empty NAND on `storage`. The label is optional and can be
    /// set later with [`Self::set_label`].
    async fn new_empty<St: VolumeStorage + 'static>(
        storage: St,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Self, Error>
    where
        Self: Sized;

    async fn open<St: VolumeStorage + 'static>(storage: St) -> Result<Self, Error>
    where
        Self: Sized;

    fn root(&self) -> Arc<dyn VolumeStorage>;

    /// Compose this immutable NAND with its writable save repository. The
    /// returned storage is fixed for the lifetime of the emulating instance.
    fn compose(&self, saves: Arc<dyn SaveRepo>) -> Result<Arc<dyn MountableStorage>, Error>;

    /// The consolespec platform ID this NAND was created for.
    fn platform(&self) -> &str;

    fn is_complete(&self, completion_criteria: NandCompletion) -> bool;

    /// What the guest may do with `path`, relative to the NAND root. Static
    /// layout policy, so it does not touch the storage.
    fn permissions(&self, path: &str) -> Permissions;

    /// Clone the base, ignoring save games.
    async fn clone<St: VolumeStorage + 'static>(&self, storage: St) -> Result<Self, Error>
    where
        Self: Sized;

    async fn metadata(&self) -> Result<VolumeInfo, Error> {
        self.root().volumeinfo().await?.ok_or(Error::UnclaimedNand)
    }

    /// Distinguishes this NAND from the user's others, e.g. for scoping saves.
    async fn id(&self) -> Result<Uuid, Error> {
        Ok(self.metadata().await?.id)
    }

    async fn label(&self) -> Result<Option<String>, Error> {
        Ok(self.metadata().await?.label)
    }

    async fn set_label(&self, label: Option<&str>) -> Result<(), Error> {
        self.root().set_label(label).await
    }

    /// Everything installed, without reading any manifest.
    async fn artifacts(&self) -> Result<Vec<ArtifactSummary>, Error> {
        self.root().artifacts().summaries().await
    }

    /// Add the artifact to the nand manifest.
    async fn record(&self, artifact: &Artifact) -> Result<(), Error> {
        self.root().artifacts().record(artifact).await
    }

    /// Delete every file `id` installed, then forget it. The record is only
    /// dropped once the files are gone, so a failed uninstall can be retried.
    async fn uninstall(&self, id: &str) -> Result<(), Error> {
        let Some(artifact) = self.root().artifacts().artifact(id).await? else {
            return Ok(());
        };

        for file in artifact.manifest() {
            self.root().remove(&file.path().to_string_lossy())?;
        }

        self.root().artifacts().forget(id).await
    }
}
