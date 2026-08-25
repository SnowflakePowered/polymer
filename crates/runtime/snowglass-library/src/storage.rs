use std::path::{Path, PathBuf};

use snowglass_consolestorage::{
    ConsoleStorage, Error, PlatformRepository, StorageKind, VolumeInfo,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_paths_do_not_depend_on_volume_identity() {
        let root = tempfile::tempdir().unwrap();
        let manager = StorageManager::new(root.path());
        let repository = root.path().join("platforms").join("PLATFORM");
        assert_eq!(manager.platform_path("PLATFORM"), repository);
    }

    #[tokio::test]
    async fn versioned_storage_persists_metadata_in_git() {
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
        let path = manager.platform_path("NINTENDO_WII");

        assert_eq!(info.label.as_deref(), Some("Wii saves"));
        let reopened = manager
            .open(StorageKind::SaveRepo, "NINTENDO_WII", info.id)
            .await
            .unwrap();
        assert_eq!(
            reopened.metadata().await.unwrap().label.as_deref(),
            Some("Wii saves")
        );
        assert!(path.join(".git").join("objects").is_dir());
    }
}
