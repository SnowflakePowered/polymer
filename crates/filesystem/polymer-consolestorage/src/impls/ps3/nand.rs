use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use super::save_repo::Ps3SaveRepo;
use crate::keys::{KeyDescription, KeyRecord};
use crate::nand::{NandCompletion, VirtualNand};
use crate::{
    Error, KeyProjection, KeyStoreStorage, MountableStorage, Permissions, PermissionsTable,
    SaveRepo, Storage, VersionedStorage, VolumeStorage, ensure_structure, requires_structure,
};

const LAYOUT: PermissionsTable = PermissionsTable::new(&[
    ("", Permissions::READ_EXECUTE),
    ("dev_hdd1", Permissions::READ_WRITE_EXECUTE),
    ("dev_hdd0/tmp", Permissions::READ_WRITE_EXECUTE),
    ("dev_hdd0/cache", Permissions::READ_WRITE_EXECUTE),
    (super::GUEST_USER_ROOT, Permissions::READ_WRITE_EXECUTE),
    (super::GUEST_SAVE_ROOT, Permissions::READ_WRITE_EXECUTE),
    ("dev_hdd0/home/00000001/exdata", Permissions::READ_EXECUTE),
]);

struct Ps3Licenses;

impl KeyProjection for Ps3Licenses {
    fn path(&self, key: &KeyDescription) -> Option<String> {
        (key.byte_length == 16 && super::rap::Rap::title_id(&key.identity.id).is_some())
            .then(|| format!("{}/exdata/{}.rap", super::GUEST_USER_ROOT, key.identity.id))
    }

    fn len(&self, key: &KeyDescription) -> u64 {
        key.byte_length
    }

    fn render(&self, key: &KeyRecord) -> Result<Vec<u8>, Error> {
        Ok(key.bytes.clone())
    }
}

pub struct Ps3Nand {
    storage: Arc<dyn VolumeStorage>,
}

#[async_trait]
impl VirtualNand for Ps3Nand {
    crate::nand::nand_depots!(super::depots::Ps3PupDepot, super::depots::Ps3PkgDepot);

    async fn new_empty<St: VolumeStorage + 'static>(
        storage: St,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Self, Error> {
        let storage: Arc<dyn VolumeStorage> = Arc::new(storage);
        ensure_structure!(
            &*storage as &dyn Storage,
            "dev_flash/",
            "dev_flash2/",
            "dev_flash3/",
            "dev_hdd0/",
            "dev_hdd0/cache/",
            "dev_hdd0/game/",
            "dev_hdd0/home/",
            "dev_hdd0/home/00000001/",
            "dev_hdd0/home/00000001/exdata/",
            "dev_hdd0/tmp/",
            "dev_hdd1/",
            "dev_hdd1/caches/",
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
        keys: Arc<dyn crate::keys::KeyReader>,
    ) -> Result<Arc<dyn MountableStorage>, Error> {
        <dyn SaveRepo>::require::<Ps3SaveRepo>(saves.as_ref(), "PlayStation 3")?;
        let mut layers: Vec<crate::storage::ComposedLayer> =
            vec![crate::storage::ComposedLayer::volatile(
                "keys",
                Arc::new(KeyStoreStorage::new(keys, Ps3Licenses).await?),
            )];
        if let Some(saves) = saves {
            layers.push(crate::storage::ComposedLayer::saves("saves", saves));
        }
        layers.push(crate::storage::ComposedLayer::volatile(
            "scratch",
            Arc::new(crate::ScratchStorage::new(self.root())),
        ));
        Ok(Arc::new(crate::ComposedStorage::with_layers(layers)?))
    }

    fn platform(&self) -> &str {
        Self::PLATFORM
    }

    fn is_complete(&self, completion: NandCompletion) -> bool {
        match completion {
            NandCompletion::Minimal => {
                Self::has_structure(&*self.storage)
                    && self
                        .storage
                        .metadata("dev_flash/sys/external/liblv2.sprx")
                        .is_ok_and(|metadata| {
                            metadata.kind == crate::EntryKind::File && metadata.len > 0
                        })
            }
            NandCompletion::Full | NandCompletion::Verified => false,
        }
    }

    fn permissions(&self, path: &str) -> Permissions {
        LAYOUT.get(path)
    }
}

impl Ps3Nand {
    pub const PLATFORM: &str = "SONY_PS3";

    pub(crate) fn has_structure(storage: &dyn Storage) -> bool {
        requires_structure!(
            storage,
            "dev_flash/",
            "dev_flash2/",
            "dev_flash3/",
            "dev_hdd0/",
            "dev_hdd0/cache/",
            "dev_hdd0/game/",
            "dev_hdd0/home/",
            "dev_hdd0/home/00000001/",
            "dev_hdd0/home/00000001/exdata/",
            "dev_hdd0/tmp/",
            "dev_hdd1/",
            "dev_hdd1/caches/",
        )
        .is_ok()
    }

    pub async fn new_save_repo(
        storage: VersionedStorage,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Arc<dyn SaveRepo>, Error> {
        Ps3SaveRepo::create(storage, id, label, super::GUEST_USER_ROOT).await
    }

    pub async fn open_save_repo(storage: VersionedStorage) -> Result<Arc<dyn SaveRepo>, Error> {
        Ps3SaveRepo::open(storage, super::GUEST_USER_ROOT).await
    }
}
