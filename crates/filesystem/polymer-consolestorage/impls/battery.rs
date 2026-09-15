use std::fmt;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use uuid::Uuid;

use crate::savegame::icons::SaveIcon;
use crate::savegame::{
    Package, SaveBlob, SaveBlobStream, SaveDepot, SaveDepotId, SaveDescriptor, SaveDescriptors,
    SaveGameSink, SaveWatcher,
};
use crate::storage::VolumeOverlay;
use crate::{
    BoxFuture, Error, MountableStorage, SaveRepo, SaveVersioning, Storage, VersionedStorage,
    VolumeStorage,
};

const SAVE_FILE: &str = "save.bin";
const SAVE_DEPOT: SaveDepotId = SaveDepotId(0);
const SETTLE_TIME: Duration = Duration::from_secs(2);

pub const PLATFORMS: &[&str] = &[
    "NINTENDO_GB",
    "NINTENDO_GBC",
    "NINTENDO_GBA",
    "NINTENDO_DS",
    "NINTENDO_NES",
    "NINTENDO_SNES",
    "NINTENDO_N64",
    "NINTENDO_VB",
    "SEGA_GG",
    "SEGA_SMS",
    "SEGA_GEN",
    "SEGA_32X",
    "BANDAI_WS",
    "BANDAI_WSC",
    "SNK_NGP",
    "SNK_NGPC",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatteryIdentitySource {
    TitleId(String),
    Serial(String),
    BinaryDigest(String),
}

impl BatteryIdentitySource {
    fn from_package(package: &Package) -> Option<Self> {
        package
            .title_id()
            .filter(|value| !value.is_empty())
            .map(|value| Self::TitleId(value.to_owned()))
            .or_else(|| {
                package
                    .serial()
                    .filter(|value| !value.is_empty())
                    .map(|value| Self::Serial(value.to_owned()))
            })
            .or_else(|| {
                package
                    .binary_digest()
                    .filter(|value| !value.is_empty())
                    .map(|value| Self::BinaryDigest(value.to_owned()))
            })
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::TitleId(_) => "titleId",
            Self::Serial(_) => "serial",
            Self::BinaryDigest(_) => "binaryDigest",
        }
    }

    fn bytes(&self) -> &[u8] {
        match self {
            Self::TitleId(value) | Self::Serial(value) | Self::BinaryDigest(value) => {
                value.as_bytes()
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatteryIdentity {
    digest: [u8; 32],
    title: String,
}

impl BatteryIdentity {
    pub fn new(platform: &str, title: impl Into<String>, source: BatteryIdentitySource) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"polymer:battery-save:v1\0");
        hasher.update(platform.as_bytes());
        hasher.update(b"\0");
        hasher.update(source.kind().as_bytes());
        hasher.update(b"\0");
        hasher.update(source.bytes());
        Self {
            digest: *hasher.finalize().as_bytes(),
            title: title.into(),
        }
    }

    pub fn from_package(
        platform: &str,
        title: impl Into<String>,
        package: &Package,
    ) -> Result<Self, Error> {
        let source = BatteryIdentitySource::from_package(package).ok_or_else(|| {
            Error::Unsupported(
                "battery save identity requires a title ID, serial, or binary digest".to_owned(),
            )
        })?;
        Ok(Self::new(platform, title, source))
    }

    /// The stable per-game profile ID. Version 8 marks the UUID payload as
    /// application-defined while retaining 122 bits of the identity digest.
    pub fn profile_id(&self) -> Uuid {
        let mut bytes: [u8; 16] = self.digest[..16].try_into().unwrap();
        bytes[6] = (bytes[6] & 0x0f) | 0x80;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Uuid::from_bytes(bytes)
    }

    pub fn title(&self) -> &str {
        &self.title
    }
}

#[derive(Clone)]
pub struct BatterySaveView {
    storage: Arc<dyn MountableStorage>,
}

impl BatterySaveView {
    pub fn storage(&self) -> &Arc<dyn MountableStorage> {
        &self.storage
    }

    pub fn entry(&self) -> &'static str {
        SAVE_FILE
    }

    pub fn depot(&self) -> SaveDepotId {
        SAVE_DEPOT
    }
}

impl fmt::Debug for BatterySaveView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BatterySaveView")
            .field("entry", &SAVE_FILE)
            .field("depot", &SAVE_DEPOT)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct BatterySaveRepo {
    root: Arc<VersionedStorage>,
    platform: String,
    title: String,
}

impl BatterySaveRepo {
    pub const SAVE_DEPOT: SaveDepotId = SAVE_DEPOT;

    pub fn supports(platform: &str) -> bool {
        PLATFORMS.contains(&platform)
    }

    pub(crate) async fn create(
        storage: VersionedStorage,
        id: Uuid,
        platform: &str,
        label: Option<String>,
    ) -> Result<Arc<dyn SaveRepo>, Error> {
        if !Self::supports(platform) {
            return Err(Error::Unsupported(format!(
                "battery saves are not supported for `{platform}`"
            )));
        }
        let title = label
            .as_deref()
            .filter(|title| !title.is_empty())
            .unwrap_or("Battery save")
            .to_owned();
        let root = Arc::new(storage);
        root.create(SAVE_FILE)?.sync()?;
        root.brand(id, platform, label.as_deref()).await?;
        Ok(Arc::new(Self {
            root,
            platform: platform.to_owned(),
            title,
        }))
    }

    pub(crate) async fn open(storage: VersionedStorage) -> Result<Arc<dyn SaveRepo>, Error> {
        let root = Arc::new(storage);
        let metadata = root.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;
        if !Self::supports(&metadata.platform) {
            return Err(Error::Unsupported(format!(
                "battery saves are not supported for `{}`",
                metadata.platform
            )));
        }
        let title = metadata
            .label
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "Battery save".to_owned());
        Ok(Arc::new(Self {
            root,
            platform: metadata.platform,
            title,
        }))
    }

    pub fn mount(self: &Arc<Self>) -> Result<BatterySaveView, Error> {
        if !self.root.exists(SAVE_FILE) {
            self.root.create(SAVE_FILE)?.sync()?;
        }
        let storage: Arc<dyn MountableStorage> = self.root.clone();
        Ok(BatterySaveView { storage })
    }

    fn describe(&self, id: SaveDepotId) -> Result<SaveDescriptor, Error> {
        if id != SAVE_DEPOT || !self.root.exists(SAVE_FILE) {
            return Err(Error::not_found("battery save"));
        }
        Ok(SaveDescriptor {
            id: SAVE_DEPOT,
            title: self.title.clone(),
            description: format!("{} battery save", self.platform),
            timestamp: self.root.metadata(SAVE_FILE)?.mtime,
            root_path: String::new(),
        })
    }
}

impl VolumeOverlay for BatterySaveRepo {
    fn inner(&self) -> &dyn VolumeStorage {
        self.root.as_ref()
    }
}

#[async_trait]
impl SaveRepo for BatterySaveRepo {
    fn versioning(&self) -> &SaveVersioning {
        self.root.versioning()
    }

    fn read_save_icon(&self, _id: SaveDepotId) -> Result<Option<SaveIcon>, Error> {
        Ok(None)
    }
}

impl SaveGameSink for BatterySaveRepo {
    fn infer_save_depot(&self, _package: &Package) -> SaveDepotId {
        SAVE_DEPOT
    }

    fn enumerate_saves(&self) -> SaveDescriptors<'_> {
        Box::new(self.describe(SAVE_DEPOT).ok().into_iter())
    }

    fn export_save<'a>(&self, id: &SaveDepotId) -> Result<SaveDepot, Error> {
        let save_descriptor = self.describe(*id)?;
        let mut bytes = Vec::new();
        self.root
            .open_read(SAVE_FILE)?
            .read_to_end(&mut bytes)
            .map_err(|source| Error::Io {
                path: SAVE_FILE.to_owned(),
                source,
            })?;
        Ok(SaveDepot {
            save_descriptor,
            blobs: vec![SaveBlob {
                target_path: PathBuf::from(SAVE_FILE),
                blake3: *blake3::hash(&bytes).as_bytes(),
                bytes,
            }],
        })
    }

    fn install_save<'a>(
        &'a self,
        mut blobs: SaveBlobStream<'a>,
    ) -> BoxFuture<'a, Result<SaveDescriptor, Error>> {
        Box::pin(async move {
            let blob = blobs.next().await.ok_or_else(|| {
                Error::InvalidPath("a battery save depot must contain one file".to_owned())
            })?;
            if blobs.next().await.is_some() || blob.target_path != PathBuf::from(SAVE_FILE) {
                return Err(Error::InvalidPath(
                    "a battery save depot must contain only save.bin".to_owned(),
                ));
            }
            let expected = blob.blake3;
            let installed = self
                .root
                .install_file(Box::new(std::io::Cursor::new(blob.bytes)), SAVE_FILE)
                .await?;
            if installed.blake3 != expected {
                return Err(Error::IntegrityMatchFailed {
                    failed: SAVE_FILE.to_owned(),
                });
            }
            self.describe(SAVE_DEPOT)
        })
    }

    fn watch_saves(&self, saves: &[SaveDepotId]) -> Result<SaveWatcher, Error> {
        let activity = self.root.watch()?.ok_or_else(|| {
            Error::Unsupported(format!("{} cannot watch battery saves", self.platform))
        })?;
        Ok(SaveWatcher::with_settle(
            saves.to_vec(),
            activity,
            SETTLE_TIME,
        ))
    }
}
