use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;

use crate::savegame::SaveGameSink;
use crate::{Error, Save, SaveVersioning, VolumeStorage};

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

    fn save(&self, id: crate::savegame::SaveDepotId) -> Save<'_>
    where
        Self: Sized,
    {
        Save::new(self, id)
    }

    async fn game_saved(&self) -> Result<(), Error> {
        self.watch_saves(&[])?.changed().await;
        let saves = self
            .enumerate_saves()
            .map(|save| save.id)
            .collect::<Vec<_>>();
        let present = saves
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        for save in saves {
            self.versioning()
                .checkpoint(self, save, crate::SaveCommitOperation::Autosave)
                .await?;
        }
        self.versioning()
            .archive_missing_saves(self, &present)
            .await?;
        Ok(())
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
