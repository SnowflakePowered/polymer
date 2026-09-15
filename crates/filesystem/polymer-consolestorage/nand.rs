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

    fn is_dirty(&self) -> Result<bool, Error> {
        self.root().is_dirty()
    }

    /// Discard uncommitted NAND contents, preserving its committed artifacts.
    fn recover(&self) -> Result<(), Error> {
        self.root().recover()
    }

    /// Construct an installation from a compatible depot without starting extraction.
    fn try_install<'a>(
        &'a self,
        _path: &std::path::Path,
    ) -> Result<crate::installable::Installation<'a>, Error> {
        Err(Error::Unsupported(
            "This NAND has no depot constructors".into(),
        ))
    }

    /// Compose this system NAND with its writable save repository. The returned
    /// storage is fixed for the lifetime of the emulating instance.
    fn compose(&self, saves: Arc<dyn SaveRepo>) -> Result<Arc<dyn MountableStorage>, Error>;

    /// The consolespec platform ID this NAND was created for.
    fn platform(&self) -> &str;

    fn is_complete(&self, completion_criteria: NandCompletion) -> bool;

    /// What the guest may do with `path`, relative to the NAND root. Static
    /// layout policy, so it does not touch the storage.
    fn permissions(&self, path: &str) -> Permissions;

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

    /// Uninstall the provided artifacts. The list of artifacts must uninstall cleanly,
    /// if there are forward dependencies, the uninstall will fail with an error.
    async fn uninstall(&self, ids: &[&str]) -> Result<(), Error> {
        self.root().artifacts().uninstall(ids).await
    }
}

macro_rules! nand_depots {
    ($($depot:ty),+ $(,)?) => {
        fn try_install<'a>(&'a self, path: &std::path::Path) -> Result<crate::installable::Installation<'a>, crate::Error> {
            use crate::installable::Depot;
            let mut errors = Vec::new();
            $(
                match <$depot>::open(path) {
                    Ok(depot) => {
                        depot.satisfies(self)?;
                        let missing = depot.validate_contents();
                        if !missing.is_empty() {
                            return Err(crate::Error::InvalidDepot { root: path.display().to_string(), missing });
                        }
                        return Ok(crate::installable::Installation::new(depot, self));
                    }
                    Err(error) => errors.push(error.to_string()),
                }
            )+
            Err(crate::Error::Unsupported(format!("No compatible depot: {}", errors.join("; "))))
        }
    };
}
pub(crate) use nand_depots;
