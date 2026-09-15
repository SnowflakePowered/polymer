use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use super::{
    CtrSaveRepo,
    layout::{Archive, Layout},
};
use crate::nand::{NandCompletion, VirtualNand};
use crate::{
    ComposedStorage, Error, MountableStorage, Permissions, SaveRepo, VersionedStorage,
    VolumeStorage,
};

/// A single mount with `nand/` and `sdmc/` subdirectories.
pub struct CtrNand {
    storage: Arc<dyn VolumeStorage>,
    platform: String,
}

impl CtrNand {
    pub const PLATFORM: &str = "NINTENDO_3DS";
    pub const NEW_PLATFORM: &str = "NINTENDO_N3DS";

    pub fn supports(platform: &str) -> bool {
        matches!(platform, Self::PLATFORM | Self::NEW_PLATFORM)
    }

    pub async fn create<St: VolumeStorage + 'static>(
        storage: St,
        id: Uuid,
        platform: &str,
        label: Option<String>,
    ) -> Result<Self, Error> {
        if !Self::supports(platform) {
            return Err(Error::Unsupported(platform.to_owned()));
        }
        let storage: Arc<dyn VolumeStorage> = Arc::new(storage);
        for path in [
            Layout::NAND_TITLE.to_owned(),
            "nand/dbs".to_owned(),
            Layout::NAND_DATA.to_owned(),
            format!("{}/title", Layout::SD),
            format!("{}/extdata/00000000", Layout::SD),
        ] {
            storage.create_dir_all(&path)?;
        }
        storage.brand(id, platform, label.as_deref()).await?;
        Ok(Self {
            storage,
            platform: platform.to_owned(),
        })
    }

    pub async fn new_save_repo(
        storage: VersionedStorage,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Arc<dyn SaveRepo>, Error> {
        CtrSaveRepo::create(storage, id, Self::PLATFORM, label).await
    }

    pub async fn open_save_repo(storage: VersionedStorage) -> Result<Arc<dyn SaveRepo>, Error> {
        CtrSaveRepo::open(storage).await
    }
}

#[async_trait]
impl VirtualNand for CtrNand {
    crate::nand::nand_depots!(super::depots::cia::CiaDepot);

    async fn new_empty<St: VolumeStorage + 'static>(
        storage: St,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Self, Error> {
        Self::create(storage, id, Self::PLATFORM, label).await
    }

    async fn open<St: VolumeStorage + 'static>(storage: St) -> Result<Self, Error> {
        let metadata = storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;
        if !Self::supports(&metadata.platform) {
            return Err(Error::Unsupported(metadata.platform));
        }
        Ok(Self {
            storage: Arc::new(storage),
            platform: metadata.platform,
        })
    }

    fn root(&self) -> Arc<dyn VolumeStorage> {
        self.storage.clone()
    }
    fn platform(&self) -> &str {
        &self.platform
    }

    fn compose(&self, saves: Arc<dyn SaveRepo>) -> Result<Arc<dyn MountableStorage>, Error> {
        if !saves.clone().into_any().is::<CtrSaveRepo>() {
            return Err(Error::Unsupported(
                "3DS NAND requires a 3DS save repository".to_owned(),
            ));
        }
        Ok(Arc::new(ComposedStorage::new(vec![saves, self.root()])?))
    }

    fn permissions(&self, path: &str) -> Permissions {
        if path.is_empty() {
            return Permissions::all();
        }
        if !Layout::valid_path(path) {
            return Permissions::empty();
        }
        if Layout::profile_path(path) || Archive::from_path(path).is_some() {
            return Permissions::all();
        }
        // Parent directories must permit first-save creation. Content remains read-only.
        if path.starts_with(&format!("{}/title/", Layout::SD))
            && path.split('/').any(|part| part == "content")
        {
            return Permissions::READ_EXECUTE;
        }
        if path == "sdmc" || path.starts_with("sdmc/") {
            return Permissions::all();
        }
        Permissions::READ_EXECUTE
    }

    fn is_complete(&self, completion: NandCompletion) -> bool {
        match completion {
            // Minimal means an HLE storage layout, not installed system firmware.
            NandCompletion::Minimal => [
                Layout::NAND_TITLE,
                "nand/dbs",
                Layout::NAND_DATA,
                Layout::SD,
            ]
            .iter()
            .all(|path| self.storage.read_dir(path, false).is_ok()),
            NandCompletion::Full | NandCompletion::Verified => false,
        }
    }
}
