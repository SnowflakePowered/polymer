use std::path::{Path, PathBuf};
use std::sync::Arc;

use snowglass_consolestorage::impls::wiiu::nand::WiiUNand;
use snowglass_consolestorage::nand::VirtualNand;
use snowglass_consolestorage::{DirectoryStorage, Error, NandMetadata, Storage};
use uuid::Uuid;

pub type Nand = Arc<dyn VirtualNand>;

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
            .join("nands")
            .join(platform)
            .join(id.simple().to_string())
    }

    pub fn supports(platform: &str) -> bool {
        matches!(platform,  "NINTENDO_WIIU")
    }

    pub async fn create(&self, platform: &str, label: Option<String>) -> Result<Nand, Error> {
        if !Self::supports(platform) {
            return Err(Error::Unsupported(format!("no nand layout for `{platform}`")));
        }

        let id = Uuid::now_v7();
        let storage = DirectoryStorage::new(self.path(platform, id)).await?;

        let nand: Nand = match platform {
            WiiUNand::PLATFORM => Arc::new(WiiUNand::new_empty(storage, id, label).await?),
            _ => unreachable!("platform support was checked"),
        };
        Ok(nand)
    }

    pub async fn open(&self, platform: &str, id: Uuid) -> Result<Nand, Error> {
        let storage = DirectoryStorage::new(self.path(platform, id)).await?;
        let metadata = storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;

        match metadata.platform.as_str() {
            WiiUNand::PLATFORM => Ok(Arc::new(WiiUNand::open(storage).await?)),
            other => Err(Error::Unsupported(format!("no nand layout for `{other}`"))),
        }
    }

    pub async fn set_label(
        &self,
        platform: &str,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Nand, Error> {
        let nand = self.open(platform, id).await?;
        nand.set_label(label.as_deref()).await?;

        Ok(nand)
    }

    pub async fn list(&self) -> Result<Vec<NandMetadata>, Error> {
        let mut nands = Vec::new();

        for platform in self.platforms().await {
            for id in self.ids(&platform).await {
                if let Ok(nand) = self.open(&platform, id).await
                    && let Ok(metadata) = nand.metadata().await
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
        Self::names(&self.root.join("nands"))
            .await
            .into_iter()
            .filter(|platform| Self::supports(platform))
            .collect()
    }

    async fn ids(&self, platform: &str) -> Vec<Uuid> {
        Self::names(&self.root.join("nands").join(platform))
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
