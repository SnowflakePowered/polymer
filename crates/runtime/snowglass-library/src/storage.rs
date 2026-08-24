use std::path::{Path, PathBuf};

use snowglass_consolestorage::{
    ConsoleStorage, ConsoleStorageBacking, Error, GitVolumeKind, PlatformRepository, StorageKind,
    VersionedStorage, VolumeInfo,
};
use uuid::Uuid;

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

    pub fn path(&self, kind: StorageKind, platform: &str, id: Uuid) -> PathBuf {
        let _ = (kind, id);
        self.root.join("platforms").join(format!("{platform}.git"))
    }

    fn repository(&self, platform: &str) -> Result<PlatformRepository, Error> {
        PlatformRepository::open(
            self.root.join("platforms").join(format!("{platform}.git")),
            platform,
        )
    }

    const fn git_kind(kind: StorageKind) -> GitVolumeKind {
        match kind {
            StorageKind::Nand => GitVolumeKind::Nand,
            StorageKind::SaveRepo => GitVolumeKind::SaveRepo,
            StorageKind::MemoryCard => GitVolumeKind::MemoryCard,
        }
    }

    pub async fn create(
        &self,
        kind: StorageKind,
        platform: &str,
        label: Option<String>,
    ) -> Result<ConsoleStorage, Error> {
        if !ConsoleStorage::supports(kind, platform) {
            return Err(Error::Unsupported(format!(
                "no {} layout for `{platform}`",
                kind.as_str()
            )));
        }

        let id = Uuid::now_v7();
        let storage = self.repository(platform)?.volume(Self::git_kind(kind), id);
        let storage = match kind {
            StorageKind::Nand => ConsoleStorageBacking::Nand(storage),
            StorageKind::SaveRepo | StorageKind::MemoryCard => {
                ConsoleStorageBacking::Versioned(VersionedStorage::from_git(storage)?)
            }
        };
        ConsoleStorage::create(kind, platform, storage, id, label).await
    }

    pub async fn open(
        &self,
        kind: StorageKind,
        platform: &str,
        id: Uuid,
    ) -> Result<ConsoleStorage, Error> {
        let storage = self.repository(platform)?.volume(Self::git_kind(kind), id);
        let storage = match kind {
            StorageKind::Nand => ConsoleStorageBacking::Nand(storage),
            StorageKind::SaveRepo | StorageKind::MemoryCard => {
                ConsoleStorageBacking::Versioned(VersionedStorage::from_git(storage)?)
            }
        };
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

    pub async fn fork(
        &self,
        kind: StorageKind,
        platform: &str,
        source: Uuid,
        label: Option<String>,
    ) -> Result<ConsoleStorage, Error> {
        if kind == StorageKind::Nand {
            return Err(Error::Unsupported(
                "NAND volumes are not save profiles".to_owned(),
            ));
        }
        let destination = Uuid::now_v7();
        self.repository(platform)?.fork_profile(
            Self::git_kind(kind),
            source,
            destination,
            label.as_deref(),
        )?;
        self.open(kind, platform, destination).await
    }

    pub async fn list(&self) -> Result<Vec<(StorageKind, VolumeInfo)>, Error> {
        let mut storages = Vec::new();

        for platform in Self::platforms(&self.root.join("platforms")).await {
            let repository = self.repository(&platform)?;
            for kind in StorageKind::ALL {
                for id in repository.ids(Self::git_kind(kind))? {
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

    pub async fn delete(&self, kind: StorageKind, platform: &str, id: Uuid) -> Result<(), Error> {
        self.repository(platform)?.delete(Self::git_kind(kind), id)
    }

    async fn platforms(path: &Path) -> Vec<String> {
        let Ok(mut listing) = tokio::fs::read_dir(path).await else {
            return Vec::new();
        };

        let mut names = Vec::new();
        while let Ok(Some(entry)) = listing.next_entry().await {
            if entry.file_type().await.is_ok_and(|kind| kind.is_dir()) {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(platform) = name.strip_suffix(".git") {
                    names.push(platform.to_owned());
                }
            }
        }

        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_storage_kinds_without_platform_policy() {
        let root = tempfile::tempdir().unwrap();
        let manager = StorageManager::new(root.path());
        let id = Uuid::now_v7();

        let repository = root.path().join("platforms").join("PLATFORM.git");
        assert_eq!(manager.path(StorageKind::Nand, "PLATFORM", id), repository);
        assert_eq!(
            manager.path(StorageKind::SaveRepo, "PLATFORM", id),
            repository
        );
        assert_eq!(
            manager.path(StorageKind::MemoryCard, "PLATFORM", id),
            repository
        );
    }

    #[tokio::test]
    async fn versioned_storage_uses_git_metadata_without_a_manifest_database() {
        let root = tempfile::tempdir().unwrap();
        let manager = StorageManager::new(root.path());
        let created = manager
            .create(
                StorageKind::SaveRepo,
                "NINTENDO_WII",
                Some("Wii saves".to_owned()),
            )
            .await
            .unwrap();
        let info = created.metadata().await.unwrap();
        let path = manager.path(StorageKind::SaveRepo, "NINTENDO_WII", info.id);

        assert_eq!(info.label.as_deref(), Some("Wii saves"));
        assert!(!path.join(".manifest.db").exists());
        assert!(!path.join(".manifest.db-wal").exists());

        let reopened = manager
            .open(StorageKind::SaveRepo, "NINTENDO_WII", info.id)
            .await
            .unwrap();
        assert_eq!(
            reopened.metadata().await.unwrap().label.as_deref(),
            Some("Wii saves")
        );
        assert!(path.join("objects").is_dir());
    }
}
