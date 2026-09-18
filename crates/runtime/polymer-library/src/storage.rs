use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use polymer_consolestorage::{
    ArtifactSummary, ConsoleStorage, Error, PlatformRepository, StorageKind, VolumeInfo,
    VolumeStorage, keys::KeyWriter,
};
use uuid::Uuid;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct StorageInspection {
    pub id: String,
    pub platform: String,
    pub minimal: bool,
    pub dirty: bool,
    pub artifacts: Vec<polymer_consolestorage::ArtifactSummary>,
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

    pub fn platform_path(&self, platform: &str) -> PathBuf {
        self.root.join("platforms").join(platform)
    }

    fn repository(&self, platform: &str) -> Result<PlatformRepository, Error> {
        PlatformRepository::open(self.platform_path(platform), platform)
    }

    pub async fn create(
        &self,
        kind: StorageKind,
        platform: &str,
        label: Option<String>,
    ) -> Result<ConsoleStorage, Error> {
        if kind == StorageKind::Battery {
            return Err(Error::Unsupported(
                "battery profiles are created automatically for each game".to_owned(),
            ));
        }
        if !ConsoleStorage::supports(kind, platform) {
            return Err(Error::Unsupported(format!(
                "no {} layout for `{platform}`",
                kind.as_str()
            )));
        }

        let id = Uuid::now_v7();
        let storage = self.repository(platform)?.volume(kind, id);
        ConsoleStorage::create(kind, platform, storage, id, label).await
    }

    /// Opens the deterministic battery profile for a game, creating it on its
    /// first launch. The caller derives `id` from the game's stable identity.
    pub async fn open_battery(
        &self,
        platform: &str,
        id: Uuid,
        label: Option<String>,
    ) -> Result<ConsoleStorage, Error> {
        if !ConsoleStorage::supports(StorageKind::Battery, platform) {
            return Err(Error::Unsupported(format!(
                "no battery layout for `{platform}`"
            )));
        }

        let repository = self.repository(platform)?;
        let storage = repository.volume(StorageKind::Battery, id);
        if repository.contains(StorageKind::Battery, id)? {
            ConsoleStorage::open(StorageKind::Battery, storage).await
        } else {
            ConsoleStorage::create(StorageKind::Battery, platform, storage, id, label).await
        }
    }

    pub async fn open(
        &self,
        kind: StorageKind,
        platform: &str,
        id: Uuid,
    ) -> Result<ConsoleStorage, Error> {
        let storage = self.repository(platform)?.volume(kind, id);
        ConsoleStorage::open(kind, storage).await
    }

    pub async fn set_label(
        &self,
        kind: StorageKind,
        platform: &str,
        id: Uuid,
        label: Option<String>,
    ) -> Result<ConsoleStorage, Error> {
        let storage = self.open(kind, platform, id).await?;
        storage.set_label(label.as_deref()).await?;
        Ok(storage)
    }

    /// Persist embedded keys before validating and extracting a depot into a NAND.
    pub async fn install(
        &self,
        platform: &str,
        id: Uuid,
        path: &Path,
        progress: impl Fn(String),
    ) -> Result<ArtifactSummary, String> {
        if !ConsoleStorage::supports(StorageKind::Nand, platform) {
            return Err("Unsupported NAND platform".into());
        }
        let ConsoleStorage::Nand(nand) = self
            .open(StorageKind::Nand, platform, id)
            .await
            .map_err(|error| error.to_string())?
        else {
            return Err("device is not a NAND".into());
        };
        let mut installation = nand.try_install(path).map_err(|error| error.to_string())?;
        let records = installation.embedded_keys();
        if !records.is_empty() {
            let store = crate::keys::KeyStore::open(self.root.join("keys"))
                .await
                .map_err(|error| error.to_string())?;
            store
                .for_platform(platform)
                .map_err(|error| error.to_string())?
                .put(records)
                .await
                .map_err(|error| error.to_string())?;
        }
        while let Some(item) = installation.next().await {
            progress(item.map_err(|error| error.to_string())?);
        }
        let artifact = installation
            .finish()
            .await
            .map_err(|error| error.to_string())?;
        Ok(ArtifactSummary::from(&artifact))
    }

    pub async fn fork(
        &self,
        kind: StorageKind,
        platform: &str,
        source: Uuid,
        label: Option<String>,
    ) -> Result<ConsoleStorage, Error> {
        if kind == StorageKind::Nand {
            return Err(Error::Unsupported(format!(
                "{} volumes cannot be forked",
                kind.as_str()
            )));
        }
        let destination = Uuid::now_v7();
        self.repository(platform)?
            .fork_profile(kind, source, destination, label.as_deref())?;
        self.open(kind, platform, destination).await
    }

    pub async fn list(&self) -> Result<Vec<(StorageKind, VolumeInfo)>, Error> {
        let mut storages = Vec::new();

        for platform in Self::platforms(&self.root.join("platforms")).await {
            let repository = self.repository(&platform)?;
            for kind in StorageKind::ALL {
                for id in repository.ids(kind)? {
                    if let Ok(storage) = self.open(kind, &platform, id).await
                        && let Ok(metadata) = storage.metadata().await
                    {
                        storages.push((kind, metadata));
                    }
                }
            }
        }

        storages.sort_by_key(|(_, metadata)| metadata.id);
        Ok(storages)
    }

    /// Read a volume's committed identity without opening its NAND or save data.
    pub async fn volume_info(
        &self,
        platform: &str,
        id: Uuid,
    ) -> Result<Option<(StorageKind, VolumeInfo)>, Error> {
        if !self.platform_path(platform).join(".git").is_dir() {
            return Ok(None);
        }
        let repository = self.repository(platform)?;
        for kind in StorageKind::ALL {
            if repository.ids(kind)?.contains(&id) {
                return Ok(repository
                    .volume(kind, id)
                    .volumeinfo()
                    .await?
                    .map(|info| (kind, info)));
            }
        }
        Ok(None)
    }

    pub async fn delete(&self, kind: StorageKind, platform: &str, id: Uuid) -> Result<(), Error> {
        self.repository(platform)?.delete(kind, id)
    }

    async fn platforms(path: &Path) -> Vec<String> {
        let Ok(mut listing) = tokio::fs::read_dir(path).await else {
            return Vec::new();
        };

        let mut names = Vec::new();
        while let Ok(Some(entry)) = listing.next_entry().await {
            if entry.file_type().await.is_ok_and(|kind| kind.is_dir())
                && entry.path().join(".git").is_dir()
            {
                let name = entry.file_name().to_string_lossy().into_owned();
                names.push(name);
            }
        }

        names
    }
}
