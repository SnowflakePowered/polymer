use async_trait::async_trait;

use crate::{Error, VolumeStorage};

#[async_trait]
pub trait SaveRepo: VolumeStorage {
    async fn game_saved(&self) -> Result<(), Error>;
}
