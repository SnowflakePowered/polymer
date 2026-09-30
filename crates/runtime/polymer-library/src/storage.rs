use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use polymer_consolestorage::{
    ArtifactSummary, ConsoleStorage, Error, PlatformRepository, StorageKind, VolumeInfo,
    VolumeStorage,
    installable::{InstallEvent, InstallEvents, InstallPhase, artifacts::ArtifactEntrypoint},
    keys::KeyWriter,
    nand::NandCompletion,
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

/// A snapshot of a running installation, reported while it progresses.
#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallStatus {
    pub title: Option<polymer_consolestorage::installable::InstallPreview>,
    pub phase: InstallPhase,
    /// The most recently extracted file.
    pub file: Option<String>,
    pub completed: u64,
    pub total: u64,
    #[serde(skip)]
    percent: u64,
}

impl InstallStatus {
    /// Fold in an event, returning whether the change is worth publishing.
    fn apply(&mut self, event: InstallEvent) -> bool {
        match event {
            InstallEvent::Title(title) => self.title = Some(title),
            InstallEvent::Phase { phase, total } => {
                (self.phase, self.completed, self.total, self.percent) = (phase, 0, total, 0);
            }
            InstallEvent::Advance(bytes) => {
                self.completed += bytes;
                let percent = self.completed * 100 / self.total.max(1);
                if percent == self.percent {
                    return false;
                }
                self.percent = percent;
            }
            InstallEvent::File(file) => {
                self.file = Some(file.display().to_string());
                return self.total == 0;
            }
        }
        true
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

    pub async fn inspect(&self, platform: &str, id: Uuid) -> Result<StorageInspection, Error> {
        let nand = self
            .open(StorageKind::Nand, platform, id)
            .await?
            .as_nand()
            .ok_or_else(|| Error::Unsupported("device is not a NAND".into()))?;
        Ok(StorageInspection {
            id: id.simple().to_string(),
            platform: nand.platform().into(),
            minimal: nand.is_complete(NandCompletion::Minimal),
            dirty: nand.is_dirty()?,
            artifacts: nand.artifacts().await?,
        })
    }

    /// Persist embedded keys before validating and extracting a depot into a NAND.
    pub async fn install(
        &self,
        platform: &str,
        id: Uuid,
        path: &Path,
        report: impl Fn(&InstallStatus) + Send + Sync + 'static,
    ) -> Result<ArtifactSummary, String> {
        let nand = self
            .open(StorageKind::Nand, platform, id)
            .await
            .map_err(|error| error.to_string())?
            .as_nand()
            .ok_or("device is not a NAND")?;
        let status = std::sync::Mutex::new(InstallStatus::default());
        let events = InstallEvents::new(move |event| {
            let mut status = status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if status.apply(event) {
                report(&status);
            }
        });
        events.phase(InstallPhase::Preparing, 0);
        let keys = crate::keys::KeyStore::open(self.root.join("keys"))
            .await
            .map_err(|error| error.to_string())?
            .for_platform(platform)
            .map_err(|error| error.to_string())?;
        let mut installation = nand
            .try_install(path, &keys, events.clone())
            .map_err(|error| error.to_string())?;
        let records = installation.embedded_keys();
        if !records.is_empty() {
            keys.put(records).await.map_err(|error| error.to_string())?;
        }
        while let Some(file) = installation.next().await {
            events.file(file.map_err(|error| error.to_string())?.into());
        }
        let artifact = installation
            .finish()
            .await
            .map_err(|error| error.to_string())?;
        Ok(ArtifactSummary::from(&artifact))
    }

    pub fn artifact_entrypoint(
        &self,
        platform: &str,
        storage: Uuid,
        artifact: &str,
    ) -> Result<(PathBuf, ArtifactEntrypoint), Error> {
        let volume = self
            .repository(platform)?
            .volume(StorageKind::Nand, storage);
        let entrypoint = volume
            .artifact_summary(artifact)?
            .and_then(|summary| summary.entrypoint)
            .ok_or_else(|| {
                Error::Unsupported(format!("{artifact} is not installed with a launch root"))
            })?;
        Ok((volume.checkout_path().to_owned(), entrypoint))
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

    /// The platform's oldest volume of `kind`, created on first use.
    async fn default_volume(&self, kind: StorageKind, platform: &str) -> Result<Uuid, Error> {
        if let Some(id) = self.repository(platform)?.ids(kind)?.into_iter().min() {
            return Ok(id);
        }
        let label = match kind {
            StorageKind::Nand => consolespec::MachineSpec::try_from(platform)
                .map(|machine| format!("{} NAND", machine.name()))
                .unwrap_or_else(|_| "Default".to_owned()),
            _ => "Default".to_owned(),
        };
        Ok(self
            .create(kind, platform, Some(label))
            .await?
            .metadata()
            .await?
            .id)
    }

    /// A launch storage selection for the platform's default device, creating its NAND,
    /// save repository or memory card on the first launch that needs one.
    pub async fn default_selection(
        &self,
        kind: StorageKind,
        platform: &str,
    ) -> Result<serde_json::Value, Error> {
        let volume = self.default_volume(kind, platform).await?.to_string();
        Ok(match kind {
            StorageKind::Nand => {
                let saves = self.default_volume(StorageKind::SaveRepo, platform).await?;
                serde_json::json!([saves.to_string(), volume])
            }
            _ => serde_json::json!(volume),
        })
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
