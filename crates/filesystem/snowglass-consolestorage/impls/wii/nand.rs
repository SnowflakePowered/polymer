use std::io::{Cursor, Read};
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

const LAYOUT: PermissionsTable = PermissionsTable::new(&[
    ("", Permissions::all()),
    ("import", Permissions::all()),
    ("tmp", Permissions::all()),
    ("shared2", Permissions::all()),
    ("sys/uid.sys", Permissions::all()),
    ("title/00000001", Permissions::READ_EXECUTE),
    ("title/00000001/00000002/data", Permissions::all()),
    (".manifest.db", Permissions::empty()),
    (".manifest.db-wal", Permissions::empty()),
]);

pub struct WiiNand {
    storage: Arc<dyn VolumeStorage>,
}

#[async_trait]
impl VirtualNand for WiiNand {
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
            "sys/uid.sys": super::wad::INITIAL_UID_SYS,
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

    fn compose(&self, saves: Arc<dyn SaveRepo>) -> Result<Arc<dyn MountableStorage>, Error> {
        Ok(Arc::new(ComposedStorage::new(vec![saves, self.root()])?))
    }

    fn platform(&self) -> &str {
        Self::PLATFORM
    }

    fn is_complete(&self, completion: NandCompletion) -> bool {
        match completion {
            NandCompletion::Minimal => {
                Self::has_structure(&*self.storage)
                    && super::wad::SharedContentMap::load(&*self.storage)
                        .is_ok_and(|map| map.is_valid())
                    && super::wad::UidMap::load(&*self.storage).is_ok_and(|map| map.is_valid())
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

    async fn clone<St: VolumeStorage + 'static>(&self, storage: St) -> Result<Self, Error> {
        let label = self.label().await?;
        let cloned = Self::new_empty(storage, Uuid::now_v7(), label).await?;
        for entry in self.storage.read_dir("", true)? {
            let path = entry.path;
            if path == ".manifest.db"
                || path.starts_with(".manifest.db-")
                || path == "import"
                || path.starts_with("import/")
                || path == "tmp"
                || path.starts_with("tmp/")
            {
                continue;
            }
            match entry.metadata.kind {
                crate::EntryKind::Directory => cloned.storage.create_dir_all(&path)?,
                crate::EntryKind::File => {
                    let mut bytes = Vec::new();
                    self.storage
                        .open_read(&path)?
                        .read_to_end(&mut bytes)
                        .map_err(|source| Error::Io {
                            path: path.clone(),
                            source,
                        })?;
                    cloned
                        .storage
                        .install_file(Box::new(Cursor::new(bytes)), &path)
                        .await?;
                }
            }
        }
        for summary in self.artifacts().await? {
            if let Some(artifact) = self.storage.artifacts().artifact(&summary.id).await? {
                cloned.record(&artifact).await?;
            }
        }
        Ok(cloned)
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

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read};

    use uuid::Uuid;

    use super::*;
    use crate::{DirectoryStorage, GitVolumeKind, PlatformRepository, VersionedStorage};

    #[tokio::test]
    async fn composes_installed_content_and_versioned_save_data() {
        let nand_dir = tempfile::tempdir().unwrap();
        let nand_storage = DirectoryStorage::new(nand_dir.path()).await.unwrap();
        let nand = WiiNand::new_empty(nand_storage, Uuid::now_v7(), None)
            .await
            .unwrap();
        for path in ["shared2/ec/shopsetu.log", "shared2/succession/shop.log"] {
            let mut log = Vec::new();
            nand.root()
                .open_read(path)
                .unwrap()
                .read_to_end(&mut log)
                .unwrap();
            assert_eq!(log, [0; 32]);
        }
        nand.root()
            .install_file(
                Box::new(Cursor::new(b"content")),
                "title/00010000/524d4345/content/00000001.app",
            )
            .await
            .unwrap();

        let saves_dir = tempfile::tempdir().unwrap();
        let saves_storage = DirectoryStorage::new(saves_dir.path()).await.unwrap();
        let saves_id = Uuid::now_v7();
        let versioned = VersionedStorage::new(saves_storage, WiiNand::PLATFORM, saves_id).unwrap();
        let saves = WiiNand::new_save_repo(versioned, saves_id, None)
            .await
            .unwrap();
        saves
            .install_file(
                Box::new(Cursor::new(b"save")),
                "title/00010000/524d4345/data/license.bin",
            )
            .await
            .unwrap();
        let mut banner = vec![0; 0xa0 + 192 * 64 * 2 + 48 * 48 * 2];
        banner[..4].copy_from_slice(b"WIBN");
        banner[8..10].copy_from_slice(&1_u16.to_be_bytes());
        saves
            .install_file(
                Box::new(Cursor::new(banner)),
                "title/00010000/524d4345/data/banner.bin",
            )
            .await
            .unwrap();

        let composed = nand.compose(saves.clone()).unwrap();
        let names = composed
            .read_dir("title/00010000/524d4345", false)
            .unwrap()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["content", "data"]);
        let mut save = Vec::new();
        composed
            .open_read("title/00010000/524d4345/data/license.bin")
            .unwrap()
            .read_to_end(&mut save)
            .unwrap();
        assert_eq!(save, b"save");
        assert!(nand.is_complete(NandCompletion::Minimal));
        assert_eq!(saves.enumerate_saves().count(), 1);
        assert!(saves.checkpoint_saves().await.unwrap());
        saves
            .remove("title/00010000/524d4345/data/banner.bin")
            .unwrap();
        assert!(
            saves
                .save_icon(crate::savegame::SaveDepotId(0x0001_0000_524d_4345))
                .await
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn routes_only_user_title_data_to_save_storage() {
        assert!(super::super::is_user_data_path(
            "title/00010000/524d4345/data/license.bin"
        ));
        assert!(!super::super::is_user_data_path(
            "title/00010000/524d4345/content/00000001.app"
        ));
        assert!(!super::super::is_user_data_path(
            "title/00000001/00000002/data/setting.txt"
        ));
    }

    #[tokio::test]
    async fn explicitly_claimed_hidden_resources_follow_the_profile() {
        let directory = tempfile::tempdir().unwrap();
        let platform =
            PlatformRepository::open(directory.path().join("NINTENDO_WII.git"), WiiNand::PLATFORM)
                .unwrap();
        let nand_id = Uuid::now_v7();
        let nand = WiiNand::new_empty(platform.volume(GitVolumeKind::Nand, nand_id), nand_id, None)
            .await
            .unwrap();
        let profile_id = Uuid::now_v7();
        let profile =
            VersionedStorage::from_git(platform.volume(GitVolumeKind::SaveRepo, profile_id))
                .unwrap();
        let saves = WiiNand::new_save_repo(profile, profile_id, None)
            .await
            .unwrap();
        let composed = nand.compose(saves.clone()).unwrap();
        let mii = composed.create("shared2/menu/FaceLib/RFL_DB.dat").unwrap();
        mii.write_all_at(0, b"mii profile").unwrap();
        let transient = composed.create("tmp/runtime-only").unwrap();
        transient.write_all_at(0, b"discard me").unwrap();

        assert!(saves.checkpoint_saves().await.unwrap());
        composed.finish("").unwrap();
        drop(composed);
        drop(saves);
        drop(nand);

        let reopened = platform.volume(GitVolumeKind::SaveRepo, profile_id);
        let mut bytes = Vec::new();
        reopened
            .open_read("shared2/menu/FaceLib/RFL_DB.dat")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"mii profile");
        assert!(
            !platform
                .volume(GitVolumeKind::Nand, nand_id)
                .exists("tmp/runtime-only")
        );
    }
}
