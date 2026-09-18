use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use super::is_user_data_path;
use super::save_repo::WiiSaveRepo;
use crate::nand::{NandCompletion, VirtualNand};
use crate::storage::VolumeStorage;
use crate::{
    ComposedStorage, Error, MountableStorage, Permissions, PermissionsTable, SaveRepo, Storage,
    VersionedStorage, ensure_structure, requires_structure,
};

use super::index;

const LAYOUT: PermissionsTable = PermissionsTable::new(&[
    ("", Permissions::all()),
    ("import", Permissions::all()),
    ("tmp", Permissions::all()),
    ("shared2", Permissions::all()),
    ("sys/uid.sys", Permissions::all()),
    ("title/00000001", Permissions::READ_EXECUTE),
    ("title/00000001/00000002/data", Permissions::all()),
]);

pub struct WiiNand {
    storage: Arc<dyn VolumeStorage>,
}

#[async_trait]
impl VirtualNand for WiiNand {
    crate::nand::nand_depots!(super::depots::wad::WiiWadDepot);

    async fn new_empty<St: VolumeStorage + 'static>(
        storage: St,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Self, Error> {
        let storage: Arc<dyn VolumeStorage> = Arc::new(storage);
        ensure_structure!(&*storage as &dyn Storage,
            "import/",
            "meta/",
            "shared1/",
            "shared1/content.map": b"",
            "shared2/",
            "shared2/ec/",
            "shared2/ec/shopsetu.log": &[0; 32],
            "shared2/menu/",
            "shared2/menu/FaceLib/",
            "shared2/succession/",
            "shared2/succession/shop.log": &[0; 32],
            "shared2/sys/",
            "shared2/wc24/",
            "sys/",
            "sys/uid.sys": index::INITIAL_UID_SYS,
            "ticket/",
            "ticket/00000001/",
            "ticket/00010001/",
            "title/",
            "title/00000001/",
            "title/00010000/",
            "title/00010001/",
            "tmp/",
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

    async fn compose(
        &self,
        saves: Option<Arc<dyn SaveRepo>>,
        keys: &dyn crate::keys::KeyReader,
    ) -> Result<Arc<dyn MountableStorage>, Error> {
        let mut files = Vec::new();
        for description in keys
            .list(&crate::keys::KeyQuery {
                namespace: Some("content".into()),
                title_id: None,
            })
            .await
            .map_err(|e| Error::Unsupported(e.to_string()))?
        {
            let Ok(title_id) = u64::from_str_radix(&description.identity.id, 16) else {
                continue;
            };
            let record = keys
                .read(&description.identity)
                .await
                .map_err(|e| Error::Unsupported(e.to_string()))?;
            let ticket = super::ticket::Ticket::synthesize(title_id, &record.bytes)?;
            files.push((
                format!("ticket/{:08x}/{:08x}.tik", title_id >> 32, title_id as u32),
                ticket,
            ));
        }
        let mut layers: Vec<Arc<dyn MountableStorage>> =
            vec![Arc::new(crate::MemoryStorage::new(files)?)];
        if let Some(saves) = saves {
            layers.push(saves);
        }
        layers.push(self.root());
        Ok(Arc::new(ComposedStorage::new(layers)?))
    }

    fn platform(&self) -> &str {
        Self::PLATFORM
    }

    fn is_complete(&self, completion: NandCompletion) -> bool {
        match completion {
            NandCompletion::Minimal => {
                Self::has_structure(&*self.storage)
                    && index::SharedContentMap::load(&*self.storage).is_ok_and(|map| map.is_valid())
                    && index::UidMap::load(&*self.storage).is_ok_and(|map| map.is_valid())
            }
            NandCompletion::Full | NandCompletion::Verified => false,
        }
    }

    fn permissions(&self, path: &str) -> Permissions {
        if is_user_data_path(path) {
            Permissions::all()
        } else {
            LAYOUT.get(path)
        }
    }
}

impl WiiNand {
    pub const PLATFORM: &str = "NINTENDO_WII";

    pub(crate) fn has_structure(storage: &dyn Storage) -> bool {
        requires_structure!(
            storage,
            "import/",
            "meta/",
            "shared1/",
            "shared1/content.map",
            "shared2/",
            "shared2/ec/",
            "shared2/ec/shopsetu.log",
            "shared2/menu/",
            "shared2/menu/FaceLib/",
            "shared2/succession/",
            "shared2/succession/shop.log",
            "shared2/sys/",
            "shared2/wc24/",
            "sys/",
            "sys/uid.sys",
            "ticket/",
            "ticket/00000001/",
            "ticket/00010001/",
            "title/",
            "title/00000001/",
            "title/00010000/",
            "title/00010001/",
            "tmp/",
        )
        .is_ok()
    }

    pub async fn new_save_repo(
        storage: VersionedStorage,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Arc<dyn SaveRepo>, Error> {
        WiiSaveRepo::create(storage, id, label).await
    }

    pub async fn open_save_repo(storage: VersionedStorage) -> Result<Arc<dyn SaveRepo>, Error> {
        WiiSaveRepo::open(storage).await
    }
}
