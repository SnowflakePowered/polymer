use std::any::Any;
use std::io::Cursor;
use std::sync::Arc;

use async_trait::async_trait;

use crate::savegame::{SaveGameSink, icons::SaveIcon};
use crate::{Error, Save, SaveAttachmentKind, SaveVersioning, VolumeStorage};

#[doc(hidden)]
pub trait DowncastSaveRepo: Any + Send + Sync {
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
}

impl<T: Any + Send + Sync> DowncastSaveRepo for T {
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
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

    async fn save_icon(&self, id: crate::savegame::SaveDepotId) -> Result<Option<SaveIcon>, Error> {
        let tip = self.versioning().tip_id(id)?;
        if let Some(tip) = tip
            && let Some(bytes) = self
                .versioning()
                .attachment(id, tip, SaveAttachmentKind::Icon)?
            && let Ok(icon) = SaveIcon::decode(&bytes)
        {
            return Ok(Some(icon));
        }

        let Some(icon) = self.read_save_icon(id)? else {
            return Ok(None);
        };
        if let Some(tip) = tip {
            let bytes = icon.encode()?;
            self.versioning()
                .attach(
                    id,
                    tip,
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
        let saves = self
            .enumerate_saves()
            .map(|save| save.id)
            .collect::<Vec<_>>();
        let present = saves
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let mut changed = false;
        for save in saves {
            changed |= self
                .versioning()
                .checkpoint(self, save, crate::SaveCommitOperation::Autosave)
                .await?
                .is_some();
        }
        changed |= self
            .versioning()
            .archive_missing_saves(self, &present)
            .await?;
        changed |= self.versioning().checkpoint_profile()?;
        Ok(changed)
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
