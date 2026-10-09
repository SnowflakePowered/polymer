use std::sync::Arc;

use polymer_consolestorage::impls::ps2::Playstation2MemoryCard;
use polymer_consolestorage::{PlatformRepository, StorageKind, VersionedStorage};
use polymer_vfs::{
    FilesystemPlan, MemoryCardStorageFactory, Permissions, PreparedFilesystem, Resource,
    ResourceOrigin, VfsPathBuf, VfsResult, Volume,
};
use uuid::Uuid;

async fn mount(platform: &PlatformRepository, formats: &[&str]) -> VfsResult<PreparedFilesystem> {
    let id = Uuid::new_v4();
    let card = Playstation2MemoryCard::new_empty(
        VersionedStorage::new(platform.volume(StorageKind::MemoryCard, id)).unwrap(),
        id,
        None,
    )
    .await
    .unwrap();
    let resource: Arc<Resource<dyn Volume>> = Arc::new(Resource::new(
        Arc::new(MemoryCardStorageFactory::new(
            Arc::new(card),
            Vec::new(),
            id,
        )),
        Permissions::ALL,
        ResourceOrigin::Scratch,
    ));
    let mut plan = FilesystemPlan::default();
    for (index, format) in formats.iter().enumerate() {
        plan.mount_storage(
            VfsPathBuf::new(format!("/card{index}")).unwrap(),
            resource.clone(),
            Permissions::ALL,
            Some((*format).to_owned()),
        );
    }
    plan.seal()
}

#[tokio::test]
async fn a_storage_can_only_be_mounted_once() {
    let directory = tempfile::tempdir().unwrap();
    let platform = PlatformRepository::open(directory.path().join("platform"), "SONY_PS2").unwrap();
    for format in ["raw", "pcsx2"] {
        let filesystem = mount(&platform, &[format]).await.unwrap();
        assert!(filesystem.materialize().is_ok());
    }
    for formats in [["raw", "raw"], ["raw", "pcsx2"]] {
        assert!(mount(&platform, &formats).await.is_err(), "{formats:?}");
    }
}
