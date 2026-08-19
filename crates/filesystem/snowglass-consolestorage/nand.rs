use crate::Error;
use crate::installable::artifacts::Artifact;
use crate::storage::Storage;
use crate::storage::manifest::{ArtifactSummary, NandIdentity};
use uuid::Uuid;

bitflags::bitflags! {
    /// Access a NAND grants for a path, in the same bit layout as
    /// `snowglass_vfs::Permissions` so a mount can pass it straight through.
    #[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
    pub struct Permissions: u8 {
        const READ = 1;
        const WRITE = 2;
        const EXECUTE = 4;

        const READ_EXECUTE = Self::READ.bits() | Self::EXECUTE.bits();
    }
}

impl Permissions {
    pub const fn can_read(self) -> bool {
        self.contains(Self::READ)
    }

    pub const fn can_write(self) -> bool {
        self.contains(Self::WRITE)
    }

    pub const fn can_execute(self) -> bool {
        self.contains(Self::EXECUTE)
    }
}

impl std::fmt::Display for Permissions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (allowed, letter) in [
            (self.can_read(), "r"),
            (self.can_write(), "w"),
            (self.can_execute(), "x"),
        ] {
            if allowed {
                formatter.write_str(letter)?;
            }
        }

        Ok(())
    }
}

/// Resolve `path` against a longest-prefix table of granted permissions.
/// Prefixes match on component boundaries, and `""` is the root.
pub fn permissions_for(path: &str, layout: &[(&str, Permissions)]) -> Permissions {
    layout
        .iter()
        .filter(|(prefix, _)| {
            prefix.is_empty()
                || path == *prefix
                || path
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(|(prefix, _)| prefix.len())
        .map_or(Permissions::empty(), |(_, permissions)| *permissions)
}

pub enum NandCompletion {
    /// The NAND is complete as far as minimally required to run a game.
    Minimal,
    /// The NAND is complete
    Full,
    /// The NAND is verified against an mtree
    Verified,
}

// TODO: lets not deal with savegames yet; they need to be mounted.
// NANDs here are just for game, os, updates etc.

pub trait VirtualNand {
    /// Lay out an empty NAND on `storage`. The label is optional and can be
    /// set later with [`Self::set_label`].
    async fn new_empty<St: Storage + 'static>(
        storage: St,
        label: Option<String>,
    ) -> Result<Self, Error>
    where
        Self: Sized;

    fn root(&self) -> &dyn Storage;

    /// The consolespec platform ID this NAND was created for.
    fn platform(&self) -> &str;

    fn is_complete(&self, completion_criteria: NandCompletion) -> bool;

    /// What the guest may do with `path`, relative to the NAND root. Static
    /// layout policy, so it does not touch the storage.
    fn permissions(&self, path: &str) -> Permissions;

    /// Clone the base, ignoring save games.
    async fn clone<St: Storage + 'static>(&self, storage: St) -> Result<Self, Error>
    where
        Self: Sized;

    /// Who this NAND is. Fails if nothing has claimed the manifest, which
    /// `new_empty` does.
    async fn identity(&self) -> Result<NandIdentity, Error> {
        self.root()
            .artifacts()
            .identity()
            .await?
            .ok_or(Error::UnclaimedNand)
    }

    /// Distinguishes this NAND from the user's others, e.g. for scoping saves.
    async fn id(&self) -> Result<Uuid, Error> {
        Ok(self.identity().await?.id)
    }

    async fn label(&self) -> Result<Option<String>, Error> {
        Ok(self.identity().await?.label)
    }

    async fn set_label(&self, label: &str) -> Result<(), Error> {
        self.root().artifacts().set_label(label).await
    }

    /// Everything installed, without reading any manifest.
    async fn artifacts(&self) -> Result<Vec<ArtifactSummary>, Error> {
        self.root().artifacts().summaries().await
    }

    /// Note that `artifact` is installed here.
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
