use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use super::VitaSaveRepo;
use crate::nand::{NandCompletion, VirtualNand};
use crate::{
    ComposedStorage, EntryKind, Error, MountableStorage, Permissions, PermissionsTable, SaveRepo,
    VersionedStorage, VolumeStorage,
};

const DIRECTORIES: &[&str] = &[
    "os0",
    "sa0",
    "vs0",
    "ur0",
    "ux0/app",
    "ux0/patch",
    "ux0/addcont",
    "ux0/license",
    "ux0/user",
    "ux0/data",
    "ux0/music",
    "ux0/picture",
    "ux0/video",
    "ux0/theme",
    "uma0/data",
    "vd0/registry",
    "vd0/network",
    "tm0",
];

const PERMISSIONS: PermissionsTable = PermissionsTable::new(&[
    ("", Permissions::READ_EXECUTE),
    (super::GUEST_USER_ROOT, Permissions::all()),
    ("ux0/data", Permissions::all()),
    ("uma0/data", Permissions::all()),
    ("vd0", Permissions::all()),
    ("tm0", Permissions::all()),
]);

pub struct VitaNand {
    storage: Arc<dyn VolumeStorage>,
}

impl VitaNand {
    pub const PLATFORM: &str = "SONY_PSV";

    pub async fn new_save_repo(
        storage: VersionedStorage,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Arc<dyn SaveRepo>, Error> {
        VitaSaveRepo::create(storage, id, label, super::GUEST_USER_ROOT).await
    }

    pub async fn open_save_repo(storage: VersionedStorage) -> Result<Arc<dyn SaveRepo>, Error> {
        VitaSaveRepo::open(storage, super::GUEST_USER_ROOT).await
    }
}

#[async_trait]
impl VirtualNand for VitaNand {
    async fn new_empty<St: VolumeStorage + 'static>(
        storage: St,
        id: Uuid,
        label: Option<String>,
    ) -> Result<Self, Error> {
        for directory in DIRECTORIES {
            storage.create_dir_all(directory)?;
        }
        storage.brand(id, Self::PLATFORM, label.as_deref()).await?;
        Ok(Self {
            storage: Arc::new(storage),
        })
    }

    async fn open<St: VolumeStorage + 'static>(storage: St) -> Result<Self, Error> {
        let metadata = storage.volumeinfo().await?.ok_or(Error::UnclaimedNand)?;
        if metadata.platform != Self::PLATFORM {
            return Err(Error::Unsupported(format!(
                "{} is not a Vita volume",
                metadata.platform
            )));
        }
        Ok(Self {
            storage: Arc::new(storage),
        })
    }

    fn root(&self) -> Arc<dyn VolumeStorage> {
        self.storage.clone()
    }

    fn platform(&self) -> &str {
        Self::PLATFORM
    }

    fn compose(&self, saves: Arc<dyn SaveRepo>) -> Result<Arc<dyn MountableStorage>, Error> {
        if !saves.clone().into_any().is::<VitaSaveRepo>() {
            return Err(Error::Unsupported(
                "Vita NAND requires a Vita save repository".to_owned(),
            ));
        }
        Ok(Arc::new(ComposedStorage::new(vec![saves, self.root()])?))
    }

    fn is_complete(&self, completion: NandCompletion) -> bool {
        match completion {
            // A layout alone cannot establish firmware readiness. Full and
            // verified completion require a firmware manifest in a future installer.
            NandCompletion::Minimal => {
                DIRECTORIES.iter().all(|path| {
                    self.storage
                        .metadata(path)
                        .is_ok_and(|metadata| metadata.kind == EntryKind::Directory)
                }) && self
                    .storage
                    .metadata("vs0/sys/external/libSceFt2.suprx")
                    .is_ok_and(|metadata| metadata.kind == EntryKind::File && metadata.len > 0)
            }
            NandCompletion::Full | NandCompletion::Verified => false,
        }
    }

    fn permissions(&self, path: &str) -> Permissions {
        PERMISSIONS.get(path)
    }
}
