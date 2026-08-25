use std::str::FromStr;
use std::sync::Arc;

use uuid::Uuid;

use crate::impls::dreamcast::DreamcastMemoryCard;
use crate::impls::gamecube::GameCubeMemoryCard;
use crate::impls::ps1::PlaystationMemoryCard;
use crate::impls::ps2::Playstation2MemoryCard;
use crate::impls::wii::nand::WiiNand;
use crate::impls::wiiu::nand::WiiUNand;
use crate::nand::VirtualNand;
use crate::{Error, GitVolumeStorage, SaveRepo, VersionedStorage, VolumeInfo, VolumeStorage};

pub type Nand = Arc<dyn VirtualNand>;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StorageKind {
    Nand,
    SaveRepo,
    MemoryCard,
}

impl StorageKind {
    pub const ALL: [Self; 3] = [Self::Nand, Self::SaveRepo, Self::MemoryCard];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nand => "nand",
            Self::SaveRepo => "saveRepo",
            Self::MemoryCard => "memoryCard",
        }
    }
}

impl FromStr for StorageKind {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "nand" => Ok(Self::Nand),
            "saveRepo" => Ok(Self::SaveRepo),
            "memoryCard" => Ok(Self::MemoryCard),
            other => Err(Error::Unsupported(format!(
                "unknown console storage kind `{other}`"
            ))),
        }
    }
}

pub enum ConsoleStorage {
    Nand(Nand),
    SaveRepo(Arc<dyn SaveRepo>),
    MemoryCard(Arc<dyn SaveRepo>),
}

impl ConsoleStorage {
    pub fn supports(kind: StorageKind, platform: &str) -> bool {
        match kind {
            StorageKind::Nand | StorageKind::SaveRepo => {
                matches!(platform, WiiNand::PLATFORM | WiiUNand::PLATFORM)
            }
            StorageKind::MemoryCard => matches!(
                platform,
                DreamcastMemoryCard::PLATFORM
                    | GameCubeMemoryCard::PLATFORM
                    | PlaystationMemoryCard::PLATFORM
                    | Playstation2MemoryCard::PLATFORM
            ),
        }
    }

    pub async fn create(
        kind: StorageKind,
        platform: &str,
        storage: GitVolumeStorage,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Self, Error> {
        match (kind, platform) {
            (StorageKind::Nand, WiiUNand::PLATFORM) => Ok(Self::Nand(Arc::new(
                WiiUNand::new_empty(storage, id, label).await?,
            ))),
            (StorageKind::Nand, WiiNand::PLATFORM) => Ok(Self::Nand(Arc::new(
                WiiNand::new_empty(storage, id, label).await?,
            ))),
            (StorageKind::SaveRepo, WiiUNand::PLATFORM) => Ok(Self::SaveRepo(
                WiiUNand::new_save_repo(VersionedStorage::new(storage)?, id, label).await?,
            )),
            (StorageKind::SaveRepo, WiiNand::PLATFORM) => Ok(Self::SaveRepo(
                WiiNand::new_save_repo(VersionedStorage::new(storage)?, id, label).await?,
            )),
            (StorageKind::MemoryCard, DreamcastMemoryCard::PLATFORM) => {
                Ok(Self::MemoryCard(Arc::new(
                    DreamcastMemoryCard::new_empty(VersionedStorage::new(storage)?, id, label)
                        .await?,
                )))
            }
            (StorageKind::MemoryCard, GameCubeMemoryCard::PLATFORM) => {
                Ok(Self::MemoryCard(Arc::new(
                    GameCubeMemoryCard::new_empty(VersionedStorage::new(storage)?, id, label)
                        .await?,
                )))
            }
            (StorageKind::MemoryCard, PlaystationMemoryCard::PLATFORM) => {
                Ok(Self::MemoryCard(Arc::new(
                    PlaystationMemoryCard::new_empty(VersionedStorage::new(storage)?, id, label)
                        .await?,
                )))
            }
            (StorageKind::MemoryCard, Playstation2MemoryCard::PLATFORM) => {
                Ok(Self::MemoryCard(Arc::new(
                    Playstation2MemoryCard::new_empty(VersionedStorage::new(storage)?, id, label)
                        .await?,
                )))
            }
            (kind, platform) => Err(Error::Unsupported(format!(
                "no {} layout for `{platform}`",
                kind.as_str()
            ))),
        }
    }

    pub async fn open(kind: StorageKind, storage: GitVolumeStorage) -> Result<Self, Error> {
        let metadata = storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;
        match (kind, metadata.platform.as_str()) {
            (StorageKind::Nand, WiiUNand::PLATFORM) => {
                Ok(Self::Nand(Arc::new(WiiUNand::open(storage).await?)))
            }
            (StorageKind::Nand, WiiNand::PLATFORM) => {
                Ok(Self::Nand(Arc::new(WiiNand::open(storage).await?)))
            }
            (StorageKind::SaveRepo, WiiUNand::PLATFORM) => Ok(Self::SaveRepo(
                WiiUNand::open_save_repo(VersionedStorage::new(storage)?).await?,
            )),
            (StorageKind::SaveRepo, WiiNand::PLATFORM) => Ok(Self::SaveRepo(
                WiiNand::open_save_repo(VersionedStorage::new(storage)?).await?,
            )),
            (StorageKind::MemoryCard, DreamcastMemoryCard::PLATFORM) => Ok(Self::MemoryCard(
                Arc::new(DreamcastMemoryCard::open(VersionedStorage::new(storage)?).await?),
            )),
            (StorageKind::MemoryCard, GameCubeMemoryCard::PLATFORM) => Ok(Self::MemoryCard(
                Arc::new(GameCubeMemoryCard::open(VersionedStorage::new(storage)?).await?),
            )),
            (StorageKind::MemoryCard, PlaystationMemoryCard::PLATFORM) => Ok(Self::MemoryCard(
                Arc::new(PlaystationMemoryCard::open(VersionedStorage::new(storage)?).await?),
            )),
            (StorageKind::MemoryCard, Playstation2MemoryCard::PLATFORM) => Ok(Self::MemoryCard(
                Arc::new(Playstation2MemoryCard::open(VersionedStorage::new(storage)?).await?),
            )),
            (other_kind, other_platform) => Err(Error::Unsupported(format!(
                "no {} layout for `{other_platform}`",
                other_kind.as_str()
            ))),
        }
    }

    pub fn save_repo(&self) -> Option<&Arc<dyn SaveRepo>> {
        match self {
            Self::Nand(_) => None,
            Self::SaveRepo(repo) | Self::MemoryCard(repo) => Some(repo),
        }
    }

    pub fn downcast<T: SaveRepo + 'static>(&self) -> Option<Arc<T>> {
        Arc::clone(self.save_repo()?).into_any().downcast().ok()
    }

    pub const fn kind(&self) -> StorageKind {
        match self {
            Self::Nand(_) => StorageKind::Nand,
            Self::SaveRepo(_) => StorageKind::SaveRepo,
            Self::MemoryCard(_) => StorageKind::MemoryCard,
        }
    }

    pub async fn metadata(&self) -> Result<VolumeInfo, Error> {
        match self {
            Self::Nand(nand) => nand.metadata().await,
            Self::SaveRepo(repo) | Self::MemoryCard(repo) => {
                repo.volumeinfo().await?.ok_or(Error::UnclaimedNand)
            }
        }
    }

    pub async fn set_label(&self, label: Option<&str>) -> Result<(), Error> {
        match self {
            Self::Nand(nand) => nand.set_label(label).await,
            Self::SaveRepo(repo) | Self::MemoryCard(repo) => {
                VolumeStorage::set_label(&**repo, label).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wii_exposes_system_and_save_volumes() {
        assert!(ConsoleStorage::supports(StorageKind::Nand, "NINTENDO_WII"));
        assert!(ConsoleStorage::supports(
            StorageKind::SaveRepo,
            "NINTENDO_WII"
        ));
        assert!(!ConsoleStorage::supports(
            StorageKind::MemoryCard,
            "NINTENDO_WII"
        ));
    }
}
