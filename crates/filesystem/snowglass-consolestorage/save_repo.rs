use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;

use crate::savegame::SaveGameSink;
use crate::{Error, VolumeStorage};

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
    async fn game_saved(&self) -> Result<(), Error> {
        self.watch_saves(&[])?.changed().await;
        Ok(())
    }
}
