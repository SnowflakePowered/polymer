use std::path::{Path, PathBuf};

use snowglass_consolestorage::{
    ConsoleStorage, ConsoleStorageBacking, DirectoryStorage, Error, StorageKind, VersionedStorage,
    VolumeInfo, VolumeStorage,
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
        self.root
            .join(kind.collection())
            .join(platform)
            .join(id.simple().to_string())
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
        let storage = DirectoryStorage::new(self.path(kind, platform, id)).await?;
        let storage = match kind {
            StorageKind::Nand => ConsoleStorageBacking::Directory(storage),
            StorageKind::SaveRepo | StorageKind::MemoryCard => {
                ConsoleStorageBacking::Versioned(VersionedStorage::new(storage, platform, id)?)
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
        let storage = DirectoryStorage::new(self.path(kind, platform, id)).await?;
        let storage = match kind {
            StorageKind::Nand => {
                let metadata = storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;
                if metadata.platform != platform {
                    return Err(Error::Unsupported(format!(
                        "`{storage:?}` is a {} volume",
                        metadata.platform
                    )));
                }
                ConsoleStorageBacking::Directory(storage)
            }
            StorageKind::SaveRepo | StorageKind::MemoryCard => {
                ConsoleStorageBacking::Versioned(VersionedStorage::new(storage, platform, id)?)
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

    pub async fn list(&self) -> Result<Vec<(StorageKind, VolumeInfo)>, Error> {
        let mut storages = Vec::new();

        for kind in StorageKind::ALL {
            for platform in Self::names(&self.root.join(kind.collection())).await {
                for id in self.ids(kind, &platform).await {
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
        let path = self.path(kind, platform, id);
        match tokio::fs::remove_dir_all(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(&path, e)),
        }
    }

    async fn ids(&self, kind: StorageKind, platform: &str) -> Vec<Uuid> {
        Self::names(&self.root.join(kind.collection()).join(platform))
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

    #[test]
    fn maps_storage_kinds_without_platform_policy() {
        let root = tempfile::tempdir().unwrap();
        let manager = StorageManager::new(root.path());
        let id = Uuid::now_v7();

        assert_eq!(
            manager.path(StorageKind::Nand, "PLATFORM", id),
            root.path()
                .join("nands")
                .join("PLATFORM")
                .join(id.simple().to_string())
        );
        assert!(
            manager
                .path(StorageKind::SaveRepo, "PLATFORM", id)
                .starts_with(root.path().join("saverepo"))
        );
        assert!(
            manager
                .path(StorageKind::MemoryCard, "PLATFORM", id)
                .starts_with(root.path().join("memorycards"))
        );
    }
}
