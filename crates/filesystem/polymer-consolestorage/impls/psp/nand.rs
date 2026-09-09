use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use super::save_repo::PspSaveRepo;
use crate::nand::{NandCompletion, VirtualNand};
use crate::{
    Error, MountableStorage, Permissions, PermissionsTable, SaveRepo, Storage, VersionedStorage,
    VolumeStorage, ensure_structure, requires_structure,
};

const LAYOUT: PermissionsTable = PermissionsTable::new(&[
    ("", Permissions::READ_EXECUTE),
    ("PSP/SAVEDATA", Permissions::all()),
]);

pub struct PspMemoryStick {
    storage: Arc<dyn VolumeStorage>,
}

#[async_trait]
impl VirtualNand for PspMemoryStick {
    async fn new_empty<St: VolumeStorage + 'static>(
        storage: St,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Self, Error> {
        let storage: Arc<dyn VolumeStorage> = Arc::new(storage);
        ensure_structure!(
            &*storage as &dyn Storage,
            "PSP/",
            "PSP/GAME/",
            "PSP/SAVEDATA/"
        )?;
        storage.brand(id, Self::PLATFORM, label.as_deref()).await?;
        Ok(Self { storage })
    }

    async fn open<St: VolumeStorage + 'static>(storage: St) -> Result<Self, Error> {
        let storage: Arc<dyn VolumeStorage> = Arc::new(storage);
        let metadata = storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;
        if metadata.platform != Self::PLATFORM {
            return Err(Error::Unsupported(format!(
                "`{storage:?}` is a {} volume",
                metadata.platform
            )));
        }
        Ok(Self { storage })
    }

    fn root(&self) -> Arc<dyn VolumeStorage> {
        self.storage.clone()
    }

    fn compose(&self, saves: Arc<dyn SaveRepo>) -> Result<Arc<dyn MountableStorage>, Error> {
        Ok(Arc::new(crate::ComposedStorage::new(vec![
            saves,
            self.root(),
        ])?))
    }

    fn platform(&self) -> &str {
        Self::PLATFORM
    }

    fn is_complete(&self, completion: NandCompletion) -> bool {
        match completion {
            NandCompletion::Minimal => Self::has_structure(&*self.storage),
            NandCompletion::Full | NandCompletion::Verified => false,
        }
    }

    fn permissions(&self, path: &str) -> Permissions {
        LAYOUT.get(path)
    }
}

impl PspMemoryStick {
    pub const PLATFORM: &str = "SONY_PSP";

    pub(crate) fn has_structure(storage: &dyn Storage) -> bool {
        requires_structure!(storage, "PSP/", "PSP/GAME/", "PSP/SAVEDATA/").is_ok()
    }

    pub async fn new_save_repo(
        storage: VersionedStorage,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Arc<dyn SaveRepo>, Error> {
        PspSaveRepo::create(storage, id, label, "PSP").await
    }

    pub async fn open_save_repo(storage: VersionedStorage) -> Result<Arc<dyn SaveRepo>, Error> {
        PspSaveRepo::open(storage, "PSP").await
    }
}
