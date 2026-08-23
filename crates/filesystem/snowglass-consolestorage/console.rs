use std::str::FromStr;
use std::sync::Arc;

use crate::impls::dreamcast::DreamcastMemoryCard;
use crate::impls::gamecube::GameCubeMemoryCard;
use crate::impls::ps1::PlaystationMemoryCard;
use crate::impls::ps2::Playstation2MemoryCard;
use crate::impls::wiiu::nand::WiiUNand;
use crate::nand::VirtualNand;
use crate::{DirectoryStorage, Error, MemoryCard, NandMetadata, SaveRepo, Storage, VolumeStorage};
use uuid::Uuid;

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

    pub const fn collection(self) -> &'static str {
        match self {
            Self::Nand => "nands",
            Self::SaveRepo => "saverepo",
            Self::MemoryCard => "memorycards",
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
    DreamcastMemoryCard(Arc<DreamcastMemoryCard>),
    GameCubeMemoryCard(Arc<GameCubeMemoryCard>),
    PlaystationMemoryCard(Arc<PlaystationMemoryCard>),
    Playstation2MemoryCard(Arc<Playstation2MemoryCard>),
}

impl ConsoleStorage {
    pub const fn kind(&self) -> StorageKind {
        match self {
            Self::Nand(_) => StorageKind::Nand,
            Self::SaveRepo(_) => StorageKind::SaveRepo,
            Self::DreamcastMemoryCard(_)
            | Self::GameCubeMemoryCard(_)
            | Self::PlaystationMemoryCard(_)
            | Self::Playstation2MemoryCard(_) => StorageKind::MemoryCard,
        }
    }

    pub async fn metadata(&self) -> Result<NandMetadata, Error> {
        match self {
            Self::Nand(nand) => nand.metadata().await,
            Self::SaveRepo(repo) => repo.volumeinfo().await?.ok_or(Error::UnclaimedNand),
            Self::DreamcastMemoryCard(card) => card.metadata().await,
            Self::GameCubeMemoryCard(card) => card.metadata().await,
            Self::PlaystationMemoryCard(card) => card.metadata().await,
            Self::Playstation2MemoryCard(card) => card.metadata().await,
        }
    }

    pub async fn set_label(&self, label: Option<&str>) -> Result<(), Error> {
        match self {
            Self::Nand(nand) => nand.set_label(label).await,
            Self::SaveRepo(repo) => Storage::set_label(&**repo, label).await,
            Self::DreamcastMemoryCard(card) => card.storage().set_label(label).await,
            Self::GameCubeMemoryCard(card) => card.storage().set_label(label).await,
            Self::PlaystationMemoryCard(card) => card.storage().set_label(label).await,
            Self::Playstation2MemoryCard(card) => card.storage().set_label(label).await,
        }
    }

    pub async fn game_saved(&self) -> Result<(), Error> {
        match self {
            Self::Nand(_) => Err(Error::Unsupported(
                "immutable NANDs do not contain saves".to_owned(),
            )),
            Self::SaveRepo(repo) => repo.game_saved().await,
            Self::DreamcastMemoryCard(card) => card.game_saved().await,
            Self::GameCubeMemoryCard(card) => card.game_saved().await,
            Self::PlaystationMemoryCard(card) => card.game_saved().await,
            Self::Playstation2MemoryCard(card) => card.game_saved().await,
        }
    }
}

pub fn supports(kind: StorageKind, platform: &str) -> bool {
    match kind {
        StorageKind::Nand | StorageKind::SaveRepo => platform == WiiUNand::PLATFORM,
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
    storage: DirectoryStorage,
    id: Uuid,
    label: Option<String>,
) -> Result<ConsoleStorage, Error> {
    match (kind, platform) {
        (StorageKind::Nand, WiiUNand::PLATFORM) => Ok(ConsoleStorage::Nand(Arc::new(
            WiiUNand::new_empty(storage, id, label).await?,
        ))),
        (StorageKind::SaveRepo, WiiUNand::PLATFORM) => Ok(ConsoleStorage::SaveRepo(
            WiiUNand::new_save_repo(storage, id, label).await?,
        )),
        (StorageKind::MemoryCard, DreamcastMemoryCard::PLATFORM) => {
            Ok(ConsoleStorage::DreamcastMemoryCard(Arc::new(
                DreamcastMemoryCard::new_empty(storage, id, label).await?,
            )))
        }
        (StorageKind::MemoryCard, GameCubeMemoryCard::PLATFORM) => {
            Ok(ConsoleStorage::GameCubeMemoryCard(Arc::new(
                GameCubeMemoryCard::new_empty(storage, id, label).await?,
            )))
        }
        (StorageKind::MemoryCard, PlaystationMemoryCard::PLATFORM) => {
            Ok(ConsoleStorage::PlaystationMemoryCard(Arc::new(
                PlaystationMemoryCard::new_empty(storage, id, label).await?,
            )))
        }
        (StorageKind::MemoryCard, Playstation2MemoryCard::PLATFORM) => {
            Ok(ConsoleStorage::Playstation2MemoryCard(Arc::new(
                Playstation2MemoryCard::new_empty(storage, id, label).await?,
            )))
        }
        _ => Err(Error::Unsupported(format!(
            "no {} layout for `{platform}`",
            kind.as_str()
        ))),
    }
}

pub async fn open(kind: StorageKind, storage: DirectoryStorage) -> Result<ConsoleStorage, Error> {
    let metadata = storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;
    match (kind, metadata.platform.as_str()) {
        (StorageKind::Nand, WiiUNand::PLATFORM) => Ok(ConsoleStorage::Nand(Arc::new(
            WiiUNand::open(storage).await?,
        ))),
        (StorageKind::SaveRepo, WiiUNand::PLATFORM) => Ok(ConsoleStorage::SaveRepo(
            WiiUNand::open_save_repo(storage).await?,
        )),
        (StorageKind::MemoryCard, DreamcastMemoryCard::PLATFORM) => {
            Ok(ConsoleStorage::DreamcastMemoryCard(Arc::new(
                DreamcastMemoryCard::open(storage).await?,
            )))
        }
        (StorageKind::MemoryCard, GameCubeMemoryCard::PLATFORM) => Ok(
            ConsoleStorage::GameCubeMemoryCard(Arc::new(GameCubeMemoryCard::open(storage).await?)),
        ),
        (StorageKind::MemoryCard, PlaystationMemoryCard::PLATFORM) => {
            Ok(ConsoleStorage::PlaystationMemoryCard(Arc::new(
                PlaystationMemoryCard::open(storage).await?,
            )))
        }
        (StorageKind::MemoryCard, Playstation2MemoryCard::PLATFORM) => {
            Ok(ConsoleStorage::Playstation2MemoryCard(Arc::new(
                Playstation2MemoryCard::open(storage).await?,
            )))
        }
        (other_kind, other_platform) => Err(Error::Unsupported(format!(
            "no {} layout for `{other_platform}`",
            other_kind.as_str()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wiiu_owns_save_repository_construction_and_opening() {
        let directory = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        let storage = DirectoryStorage::new(directory.path()).await.unwrap();
        let created = create(
            StorageKind::SaveRepo,
            WiiUNand::PLATFORM,
            storage,
            id,
            Some("Saves".to_owned()),
        )
        .await
        .unwrap();
        let ConsoleStorage::SaveRepo(repo) = created else {
            unreachable!()
        };
        assert!(repo.exists("usr/save"));
        assert_eq!(repo.volumeinfo().await.unwrap().unwrap().id, id);

        drop(repo);
        let storage = DirectoryStorage::new(directory.path()).await.unwrap();
        let reopened = open(StorageKind::SaveRepo, storage).await.unwrap();
        assert!(matches!(reopened, ConsoleStorage::SaveRepo(_)));
    }
}
