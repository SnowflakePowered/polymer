use std::path::{Path, PathBuf};
use std::sync::Arc;

use snowglass_consolestorage::impls::dreamcast::DreamcastMemoryCard;
use snowglass_consolestorage::impls::gamecube::GameCubeMemoryCard;
use snowglass_consolestorage::impls::ps1::PlaystationMemoryCard;
use snowglass_consolestorage::impls::ps2::Playstation2MemoryCard;
use snowglass_consolestorage::impls::wiiu::nand::WiiUNand;
use snowglass_consolestorage::nand::VirtualNand;
use snowglass_consolestorage::{DirectoryStorage, Error, MemoryCard, NandMetadata, VolumeStorage};
use uuid::Uuid;

pub type Nand = Arc<dyn VirtualNand>;

pub enum ConsoleStorage {
    Nand(Nand),
    DreamcastMemoryCard(Arc<DreamcastMemoryCard>),
    GameCubeMemoryCard(Arc<GameCubeMemoryCard>),
    PlaystationMemoryCard(Arc<PlaystationMemoryCard>),
    Playstation2MemoryCard(Arc<Playstation2MemoryCard>),
}

impl ConsoleStorage {
    pub async fn metadata(&self) -> Result<NandMetadata, Error> {
        match self {
            Self::Nand(nand) => nand.metadata().await,
            Self::DreamcastMemoryCard(card) => card.metadata().await,
            Self::GameCubeMemoryCard(card) => card.metadata().await,
            Self::PlaystationMemoryCard(card) => card.metadata().await,
            Self::Playstation2MemoryCard(card) => card.metadata().await,
        }
    }

    pub async fn set_label(&self, label: Option<&str>) -> Result<(), Error> {
        match self {
            Self::Nand(nand) => nand.set_label(label).await,
            Self::DreamcastMemoryCard(card) => card.storage().set_label(label).await,
            Self::GameCubeMemoryCard(card) => card.storage().set_label(label).await,
            Self::PlaystationMemoryCard(card) => card.storage().set_label(label).await,
            Self::Playstation2MemoryCard(card) => card.storage().set_label(label).await,
        }
    }

    pub async fn game_saved(&self) -> Result<(), Error> {
        match self {
            Self::Nand(nand) => nand.game_saved().await,
            Self::DreamcastMemoryCard(card) => card.game_saved().await,
            Self::GameCubeMemoryCard(card) => card.game_saved().await,
            Self::PlaystationMemoryCard(card) => card.game_saved().await,
            Self::Playstation2MemoryCard(card) => card.game_saved().await,
        }
    }
}

pub struct StorageManager {
    root: PathBuf,
}

impl StorageManager {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, platform: &str, id: Uuid) -> PathBuf {
        self.root
            .join(match platform {
                DreamcastMemoryCard::PLATFORM
                | GameCubeMemoryCard::PLATFORM
                | PlaystationMemoryCard::PLATFORM
                | Playstation2MemoryCard::PLATFORM => "memorycards",
                _ => "nands",
            })
            .join(platform)
            .join(id.simple().to_string())
    }

    pub fn supports(platform: &str) -> bool {
        matches!(
            platform,
            WiiUNand::PLATFORM
                | DreamcastMemoryCard::PLATFORM
                | GameCubeMemoryCard::PLATFORM
                | PlaystationMemoryCard::PLATFORM
                | Playstation2MemoryCard::PLATFORM
        )
    }

    pub async fn create(
        &self,
        platform: &str,
        label: Option<String>,
    ) -> Result<ConsoleStorage, Error> {
        if !Self::supports(platform) {
            return Err(Error::Unsupported(format!(
                "no console storage layout for `{platform}`"
            )));
        }

        let id = Uuid::now_v7();
        let storage = DirectoryStorage::new(self.path(platform, id)).await?;

        let storage = match platform {
            WiiUNand::PLATFORM => {
                ConsoleStorage::Nand(Arc::new(WiiUNand::new_empty(storage, id, label).await?))
            }
            DreamcastMemoryCard::PLATFORM => ConsoleStorage::DreamcastMemoryCard(Arc::new(
                DreamcastMemoryCard::new_empty(storage, id, label).await?,
            )),
            GameCubeMemoryCard::PLATFORM => ConsoleStorage::GameCubeMemoryCard(Arc::new(
                GameCubeMemoryCard::new_empty(storage, id, label).await?,
            )),
            PlaystationMemoryCard::PLATFORM => ConsoleStorage::PlaystationMemoryCard(Arc::new(
                PlaystationMemoryCard::new_empty(storage, id, label).await?,
            )),
            Playstation2MemoryCard::PLATFORM => ConsoleStorage::Playstation2MemoryCard(Arc::new(
                Playstation2MemoryCard::new_empty(storage, id, label).await?,
            )),
            _ => unreachable!("platform support was checked"),
        };
        Ok(storage)
    }

    pub async fn open(&self, platform: &str, id: Uuid) -> Result<ConsoleStorage, Error> {
        let storage = DirectoryStorage::new(self.path(platform, id)).await?;
        let metadata = storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;

        match metadata.platform.as_str() {
            WiiUNand::PLATFORM => Ok(ConsoleStorage::Nand(Arc::new(
                WiiUNand::open(storage).await?,
            ))),
            DreamcastMemoryCard::PLATFORM => Ok(ConsoleStorage::DreamcastMemoryCard(Arc::new(
                DreamcastMemoryCard::open(storage).await?,
            ))),
            GameCubeMemoryCard::PLATFORM => Ok(ConsoleStorage::GameCubeMemoryCard(Arc::new(
                GameCubeMemoryCard::open(storage).await?,
            ))),
            PlaystationMemoryCard::PLATFORM => Ok(ConsoleStorage::PlaystationMemoryCard(Arc::new(
                PlaystationMemoryCard::open(storage).await?,
            ))),
            Playstation2MemoryCard::PLATFORM => Ok(ConsoleStorage::Playstation2MemoryCard(
                Arc::new(Playstation2MemoryCard::open(storage).await?),
            )),
            other => Err(Error::Unsupported(format!(
                "no console storage layout for `{other}`"
            ))),
        }
    }

    pub async fn set_label(
        &self,
        platform: &str,
        id: Uuid,
        label: Option<String>,
    ) -> Result<ConsoleStorage, Error> {
        let storage = self.open(platform, id).await?;
        storage.set_label(label.as_deref()).await?;

        Ok(storage)
    }

    pub async fn list(&self) -> Result<Vec<NandMetadata>, Error> {
        let mut nands = Vec::new();

        for platform in self.platforms().await {
            for id in self.ids(&platform).await {
                if let Ok(storage) = self.open(&platform, id).await
                    && let Ok(metadata) = storage.metadata().await
                {
                    nands.push(metadata);
                }
            }
        }

        nands.sort_by_key(|metadata| metadata.id);
        Ok(nands)
    }

    pub async fn delete(&self, platform: &str, id: Uuid) -> Result<(), Error> {
        let path = self.path(platform, id);
        match tokio::fs::remove_dir_all(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    async fn platforms(&self) -> Vec<String> {
        vec![
            WiiUNand::PLATFORM.to_owned(),
            DreamcastMemoryCard::PLATFORM.to_owned(),
            GameCubeMemoryCard::PLATFORM.to_owned(),
            PlaystationMemoryCard::PLATFORM.to_owned(),
            Playstation2MemoryCard::PLATFORM.to_owned(),
        ]
    }

    async fn ids(&self, platform: &str) -> Vec<Uuid> {
        let collection = match platform {
            DreamcastMemoryCard::PLATFORM
            | GameCubeMemoryCard::PLATFORM
            | PlaystationMemoryCard::PLATFORM
            | Playstation2MemoryCard::PLATFORM => self.root.join("memorycards"),
            _ => self.root.join("nands"),
        };
        Self::names(&collection.join(platform))
            .await
            .iter()
            .filter_map(|name| Uuid::parse_str(name).ok())
            .collect()
    }

    async fn names(path: &Path) -> Vec<String> {
        let Ok(mut listing) = tokio::fs::read_dir(path).await else {
            return Vec::new();
        };

        let mut names = Vec::new();
        while let Ok(Some(entry)) = listing.next_entry().await {
            if entry.file_type().await.is_ok_and(|kind| kind.is_dir()) {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }

        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creates_and_reopens_playstation_memory_cards() {
        let root = tempfile::tempdir().unwrap();
        let manager = StorageManager::new(root.path());
        let created = manager
            .create(PlaystationMemoryCard::PLATFORM, Some("PS1 Card".to_owned()))
            .await
            .unwrap();
        let metadata = created.metadata().await.unwrap();
        assert_eq!(metadata.platform, PlaystationMemoryCard::PLATFORM);
        assert_eq!(metadata.label.as_deref(), Some("PS1 Card"));
        assert!(
            manager
                .path(&metadata.platform, metadata.id)
                .starts_with(root.path().join("memorycards"))
        );

        let reopened = manager
            .open(PlaystationMemoryCard::PLATFORM, metadata.id)
            .await
            .unwrap();
        assert!(matches!(reopened, ConsoleStorage::PlaystationMemoryCard(_)));
    }

    #[tokio::test]
    async fn creates_and_reopens_gamecube_memory_cards() {
        let root = tempfile::tempdir().unwrap();
        let manager = StorageManager::new(root.path());
        let created = manager
            .create(
                GameCubeMemoryCard::PLATFORM,
                Some("GameCube Card".to_owned()),
            )
            .await
            .unwrap();
        let metadata = created.metadata().await.unwrap();
        assert_eq!(metadata.platform, GameCubeMemoryCard::PLATFORM);
        assert!(
            manager
                .path(&metadata.platform, metadata.id)
                .starts_with(root.path().join("memorycards"))
        );

        let reopened = manager
            .open(GameCubeMemoryCard::PLATFORM, metadata.id)
            .await
            .unwrap();
        assert!(matches!(reopened, ConsoleStorage::GameCubeMemoryCard(_)));
    }

    #[tokio::test]
    async fn creates_and_reopens_dreamcast_memory_cards() {
        let root = tempfile::tempdir().unwrap();
        let manager = StorageManager::new(root.path());
        let created = manager
            .create(
                DreamcastMemoryCard::PLATFORM,
                Some("Dreamcast VMU".to_owned()),
            )
            .await
            .unwrap();
        let metadata = created.metadata().await.unwrap();
        assert_eq!(metadata.platform, DreamcastMemoryCard::PLATFORM);
        assert_eq!(metadata.label.as_deref(), Some("Dreamcast VMU"));
        assert!(
            manager
                .path(&metadata.platform, metadata.id)
                .starts_with(root.path().join("memorycards"))
        );

        let reopened = manager
            .open(DreamcastMemoryCard::PLATFORM, metadata.id)
            .await
            .unwrap();
        assert!(matches!(reopened, ConsoleStorage::DreamcastMemoryCard(_)));
    }
}
