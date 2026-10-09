use std::any::Any;
use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::savegame::{Package, SaveGameSink, icons::SaveIcon};
use crate::{AttachmentScope, Error, Save, SaveAttachmentKind, SaveVersioning, VolumeStorage};

/// Asks whether a save may be checkpointed now. Returning `false` defers the save,
/// with its pending changes, to a later checkpoint.
pub type CheckpointGate =
    Box<dyn Fn(CheckpointRequest) -> crate::BoxFuture<'static, bool> + Send + Sync>;

#[derive(Clone, Debug)]
pub struct CheckpointRequest {
    pub save: crate::savegame::SaveDepotId,
    /// The save's directory within its storage, or empty for a save that no longer exists.
    pub root: String,
    pub changed: Vec<ChangedEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ChangedEntry {
    pub path: String,
    pub kind: ChangeKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    File,
    Directory,
    Removed,
}

#[doc(hidden)]
pub trait DowncastSaveRepo: Any + Send + Sync {
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
}

impl<T: Any + Send + Sync> DowncastSaveRepo for T {
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl dyn SaveRepo {
    pub(crate) fn require<R: SaveRepo>(
        saves: Option<&Arc<Self>>,
        console: &str,
    ) -> Result<(), Error> {
        if saves.is_some_and(|saves| !saves.clone().into_any().is::<R>()) {
            return Err(Error::Unsupported(format!(
                "{console} NAND requires a {console} save repository"
            )));
        }
        Ok(())
    }
}

#[async_trait]
pub trait SaveRepo: VolumeStorage + SaveGameSink + DowncastSaveRepo {
    fn versioning(&self) -> &SaveVersioning;

    #[doc(hidden)]
    fn read_save_icon(&self, id: crate::savegame::SaveDepotId) -> Result<Option<SaveIcon>, Error>;

    /// How many blocks the save occupies in the console's own save manager,
    /// for consoles that count saves in blocks rather than bytes.
    fn blocks(&self, id: crate::savegame::SaveDepotId) -> Result<Option<u64>, Error> {
        let _ = id;
        Ok(None)
    }

    /// Record what the launched game says about its saves, for consoles whose
    /// saves do not describe themselves.
    async fn describe_launch(&self, package: &Package, path: &Path) -> Result<(), Error> {
        let _ = (package, path);
        Ok(())
    }

    async fn save_icon(&self, id: crate::savegame::SaveDepotId) -> Result<Option<SaveIcon>, Error> {
        let tip = self.versioning().tip_id(id)?;
        for scope in tip
            .map(AttachmentScope::Commit)
            .into_iter()
            .chain([AttachmentScope::History])
        {
            if let Some(bytes) =
                self.versioning()
                    .attachment(id, scope, SaveAttachmentKind::Icon)?
                && let Ok(icon) = SaveIcon::decode(&bytes)
            {
                return Ok(Some(icon));
            }
        }

        let Some(icon) = self.read_save_icon(id)? else {
            return Ok(None);
        };
        if let Some(tip) = tip {
            let bytes = icon.encode()?;
            self.versioning()
                .attach(
                    id,
                    AttachmentScope::Commit(tip),
                    SaveAttachmentKind::Icon,
                    Box::new(Cursor::new(bytes)),
                )
                .await?;
        }
        Ok(Some(icon))
    }

    fn save(&self, id: crate::savegame::SaveDepotId) -> Save<'_>
    where
        Self: Sized,
    {
        Save::new(self, id)
    }

    /// Record the current state of every save and archive histories whose
    /// canonical save has disappeared.
    async fn checkpoint_saves(&self) -> Result<bool, Error> {
        self.checkpoint_saves_gated(None).await
    }

    /// Like [`SaveRepo::checkpoint_saves`], but asks `gate` before checkpointing a save
    /// with pending changes. A save the gate rejects keeps its changes pending.
    async fn checkpoint_saves_gated(&self, gate: Option<&CheckpointGate>) -> Result<bool, Error> {
        self.versioning().recover(self).await?;
        self.versioning().checkpoint_all(self, gate).await
    }

    fn recently_deleted(&self) -> Result<Vec<crate::DeletedSaveInfo>, Error> {
        self.versioning().recently_deleted()
    }

    async fn restore_deleted(
        &self,
        deletion_id: uuid::Uuid,
    ) -> Result<crate::SaveCommitInfo, Error> {
        self.versioning().restore_deleted(self, deletion_id).await
    }
}
