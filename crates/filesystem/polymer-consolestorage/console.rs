use std::str::FromStr;
use std::sync::Arc;

use uuid::Uuid;

use crate::impls::battery::BatterySaveRepo;
use crate::impls::ctr::{CtrNand, CtrSaveRepo};
use crate::impls::dreamcast::DreamcastMemoryCard;
use crate::impls::gamecube::GameCubeMemoryCard;
use crate::impls::ps1::PlaystationMemoryCard;
use crate::impls::ps2::Playstation2MemoryCard;
use crate::impls::ps3::Ps3Nand;
use crate::impls::psp::PspMemoryStick;
use crate::impls::vita::VitaNand;
use crate::impls::wii::nand::WiiNand;
use crate::impls::wiiu::nand::WiiUNand;
use crate::nand::VirtualNand;
use crate::{Error, GitVolumeStorage, SaveRepo, VersionedStorage, VolumeInfo, VolumeStorage};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum StorageKind {
    Nand,
    SaveRepo,
    MemoryCard,
    Battery,
}

impl StorageKind {
    pub const ALL: [Self; 4] = [Self::Nand, Self::SaveRepo, Self::MemoryCard, Self::Battery];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nand => "nand",
            Self::SaveRepo => "saveRepo",
            Self::MemoryCard => "memoryCard",
            Self::Battery => "battery",
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
            "battery" => Ok(Self::Battery),
            other => Err(Error::Unsupported(format!(
                "unknown console storage kind `{other}`"
            ))),
        }
    }
}

pub enum ConsoleStorage {
    Nand(Arc<dyn VirtualNand>),
    SaveRepo(Arc<dyn SaveRepo>),
    MemoryCard(Arc<dyn SaveRepo>),
    Battery(Arc<dyn SaveRepo>),
}

impl ConsoleStorage {
    /// The consolespec storage device a platform's default volume is mounted as, and
    /// that volume's kind. A NAND device also carries the platform's save repository.
    pub fn default_device(platform: &str) -> Option<(&'static str, StorageKind)> {
        Some(match platform {
            WiiNand::PLATFORM => ("nand", StorageKind::Nand),
            CtrNand::PLATFORM | CtrNand::NEW_PLATFORM => ("ctrnand", StorageKind::Nand),
            WiiUNand::PLATFORM => ("mlc", StorageKind::Nand),
            VitaNand::PLATFORM => ("emmc", StorageKind::Nand),
            GameCubeMemoryCard::PLATFORM => ("slotA", StorageKind::MemoryCard),
            DreamcastMemoryCard::PLATFORM => ("vmu", StorageKind::MemoryCard),
            PlaystationMemoryCard::PLATFORM => ("card1", StorageKind::MemoryCard),
            Playstation2MemoryCard::PLATFORM => ("mem1", StorageKind::MemoryCard),
            _ => return None,
        })
    }

    pub fn supports(kind: StorageKind, platform: &str) -> bool {
        match kind {
            StorageKind::Nand | StorageKind::SaveRepo => {
                matches!(
                    platform,
                    WiiNand::PLATFORM
                        | CtrNand::PLATFORM
                        | CtrNand::NEW_PLATFORM
                        | WiiUNand::PLATFORM
                        | Ps3Nand::PLATFORM
                        | PspMemoryStick::PLATFORM
                        | VitaNand::PLATFORM
                )
            }
            StorageKind::MemoryCard => matches!(
                platform,
                DreamcastMemoryCard::PLATFORM
                    | GameCubeMemoryCard::PLATFORM
                    | PlaystationMemoryCard::PLATFORM
                    | Playstation2MemoryCard::PLATFORM
            ),
            StorageKind::Battery => BatterySaveRepo::supports(platform),
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
            (StorageKind::Nand, VitaNand::PLATFORM) => Ok(Self::Nand(Arc::new(
                VitaNand::new_empty(storage, id, label).await?,
            ))),
            (StorageKind::SaveRepo, VitaNand::PLATFORM) => Ok(Self::SaveRepo(
                VitaNand::new_save_repo(VersionedStorage::new(storage)?, id, label).await?,
            )),
            (StorageKind::Nand, platform) if CtrNand::supports(platform) => Ok(Self::Nand(
                Arc::new(CtrNand::create(storage, id, platform, label).await?),
            )),
            (StorageKind::SaveRepo, platform) if CtrNand::supports(platform) => Ok(Self::SaveRepo(
                CtrSaveRepo::create(VersionedStorage::new(storage)?, id, platform, label).await?,
            )),
            (StorageKind::Nand, WiiUNand::PLATFORM) => Ok(Self::Nand(Arc::new(
                WiiUNand::new_empty(storage, id, label).await?,
            ))),
            (StorageKind::Nand, WiiNand::PLATFORM) => Ok(Self::Nand(Arc::new(
                WiiNand::new_empty(storage, id, label).await?,
            ))),
            (StorageKind::Nand, Ps3Nand::PLATFORM) => Ok(Self::Nand(Arc::new(
                Ps3Nand::new_empty(storage, id, label).await?,
            ))),
            (StorageKind::Nand, PspMemoryStick::PLATFORM) => Ok(Self::Nand(Arc::new(
                PspMemoryStick::new_empty(storage, id, label).await?,
            ))),
            (StorageKind::SaveRepo, WiiUNand::PLATFORM) => Ok(Self::SaveRepo(
                WiiUNand::new_save_repo(VersionedStorage::new(storage)?, id, label).await?,
            )),
            (StorageKind::SaveRepo, WiiNand::PLATFORM) => Ok(Self::SaveRepo(
                WiiNand::new_save_repo(VersionedStorage::new(storage)?, id, label).await?,
            )),
            (StorageKind::SaveRepo, Ps3Nand::PLATFORM) => Ok(Self::SaveRepo(
                Ps3Nand::new_save_repo(VersionedStorage::new(storage)?, id, label).await?,
            )),
            (StorageKind::SaveRepo, PspMemoryStick::PLATFORM) => Ok(Self::SaveRepo(
                PspMemoryStick::new_save_repo(VersionedStorage::new(storage)?, id, label).await?,
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
            (StorageKind::Battery, platform) if BatterySaveRepo::supports(platform) => {
                Ok(Self::Battery(
                    BatterySaveRepo::create(VersionedStorage::new(storage)?, id, platform, label)
                        .await?,
                ))
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
            (StorageKind::Nand, VitaNand::PLATFORM) => {
                Ok(Self::Nand(Arc::new(VitaNand::open(storage).await?)))
            }
            (StorageKind::SaveRepo, VitaNand::PLATFORM) => Ok(Self::SaveRepo(
                VitaNand::open_save_repo(VersionedStorage::new(storage)?).await?,
            )),
            (StorageKind::Nand, platform) if CtrNand::supports(platform) => {
                Ok(Self::Nand(Arc::new(CtrNand::open(storage).await?)))
            }
            (StorageKind::SaveRepo, platform) if CtrNand::supports(platform) => Ok(Self::SaveRepo(
                CtrNand::open_save_repo(VersionedStorage::new(storage)?).await?,
            )),
            (StorageKind::Nand, WiiUNand::PLATFORM) => {
                Ok(Self::Nand(Arc::new(WiiUNand::open(storage).await?)))
            }
            (StorageKind::Nand, WiiNand::PLATFORM) => {
                Ok(Self::Nand(Arc::new(WiiNand::open(storage).await?)))
            }
            (StorageKind::Nand, Ps3Nand::PLATFORM) => {
                Ok(Self::Nand(Arc::new(Ps3Nand::open(storage).await?)))
            }
            (StorageKind::Nand, PspMemoryStick::PLATFORM) => {
                Ok(Self::Nand(Arc::new(PspMemoryStick::open(storage).await?)))
            }
            (StorageKind::SaveRepo, WiiUNand::PLATFORM) => Ok(Self::SaveRepo(
                WiiUNand::open_save_repo(VersionedStorage::new(storage)?).await?,
            )),
            (StorageKind::SaveRepo, WiiNand::PLATFORM) => Ok(Self::SaveRepo(
                WiiNand::open_save_repo(VersionedStorage::new(storage)?).await?,
            )),
            (StorageKind::SaveRepo, Ps3Nand::PLATFORM) => Ok(Self::SaveRepo(
                Ps3Nand::open_save_repo(VersionedStorage::new(storage)?).await?,
            )),
            (StorageKind::SaveRepo, PspMemoryStick::PLATFORM) => Ok(Self::SaveRepo(
                PspMemoryStick::open_save_repo(VersionedStorage::new(storage)?).await?,
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
            (StorageKind::Battery, platform) if BatterySaveRepo::supports(platform) => Ok(
                Self::Battery(BatterySaveRepo::open(VersionedStorage::new(storage)?).await?),
            ),
            (other_kind, other_platform) => Err(Error::Unsupported(format!(
                "no {} layout for `{other_platform}`",
                other_kind.as_str()
            ))),
        }
    }

    pub fn as_nand(&self) -> Option<Arc<dyn VirtualNand>> {
        match self {
            Self::Nand(nand) => Some(nand.clone()),
            _ => None,
        }
    }

    pub fn save_repo(&self) -> Option<&Arc<dyn SaveRepo>> {
        match self {
            Self::Nand(_) => None,
            Self::SaveRepo(repo) | Self::MemoryCard(repo) | Self::Battery(repo) => Some(repo),
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
            Self::Battery(_) => StorageKind::Battery,
        }
    }

    pub async fn metadata(&self) -> Result<VolumeInfo, Error> {
        match self {
            Self::Nand(nand) => nand.metadata().await,
            Self::SaveRepo(repo) | Self::MemoryCard(repo) | Self::Battery(repo) => {
                repo.volumeinfo().await?.ok_or(Error::UnclaimedNand)
            }
        }
    }

    pub async fn set_label(&self, label: Option<&str>) -> Result<(), Error> {
        match self {
            Self::Nand(nand) => nand.set_label(label).await,
            Self::SaveRepo(repo) | Self::MemoryCard(repo) | Self::Battery(repo) => {
                VolumeStorage::set_label(&**repo, label).await
            }
        }
    }
}
