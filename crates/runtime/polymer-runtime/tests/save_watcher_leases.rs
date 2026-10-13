use std::sync::Arc;
use std::time::{Duration, Instant};

use polymer_consolestorage::StorageKind;
use polymer_consolestorage::impls::wii::WiiNand;
use polymer_library::StorageManager;
use polymer_vfs::{
    ExclusiveResourceKey, FilesystemPlan, FinalizationReason, OpenOptions, Permissions, Resource,
    ResourceOrigin, VfsPathBuf,
};

#[tokio::test]
async fn failed_final_checkpoint_retains_nand_and_profile_until_retry_finishes() {
    let directory = tempfile::tempdir().unwrap();
    let manager = Arc::new(StorageManager::new(directory.path()));
    let nand = manager
        .create(StorageKind::Nand, WiiNand::PLATFORM, None)
        .await
        .unwrap();
    let profile = manager
        .create(StorageKind::SaveRepo, WiiNand::PLATFORM, None)
        .await
        .unwrap();
    let nand_id = nand.metadata().await.unwrap().id;
    let profile_id = profile.metadata().await.unwrap().id;
    let saves = profile.save_repo().unwrap();
    let save_path = "title/00010001/41424344/data/save.bin";
    saves.install_bytes(b"original", save_path).unwrap();
    saves.set_label(Some("baseline")).await.unwrap();
    let (notifications, _received) = tokio::sync::mpsc::unbounded_channel();
    let factories = polymer_runtime::launch_storage(
        Some(manager.clone()),
        None,
        &["onLaunch".to_owned()],
        &[serde_json::json!({
            "console": WiiNand::PLATFORM,
            "storage": { "nand": [profile_id, nand_id] },
        })],
        notifications,
        &polymer_runtime::CheckpointSession::default(),
    )
    .await
    .unwrap();
    let mut plan = FilesystemPlan::default();
    plan.mount_tree(
        VfsPathBuf::root(),
        Arc::new(Resource::new(
            factories["nand"].clone(),
            Permissions::ALL,
            ResourceOrigin::Scratch,
        )),
        Permissions::ALL,
    );
    let filesystem = plan.seal().unwrap();
    filesystem.materialize().unwrap();
    let lock = manager.platform_path(WiiNand::PLATFORM).join(format!(
        ".git/refs/consoles/profiles/v2/{}.lock",
        profile_id.simple()
    ));
    std::fs::write(&lock, b"held").unwrap();
    let file = filesystem
        .open(
            &VfsPathBuf::new(format!("/{save_path}")).unwrap(),
            OpenOptions {
                write: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(file.write(0, b"updated!").unwrap(), 8);
    drop(file);
    assert!(
        filesystem
            .finalize(FinalizationReason::ProcessExited)
            .is_err()
    );
    assert!(filesystem.finalize(FinalizationReason::Cancelled).is_err());
    drop(filesystem);
    for id in [profile_id, nand_id] {
        assert!(ExclusiveResourceKey::Resource(id).acquire().is_err());
    }
    std::fs::remove_file(lock).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut guards = Vec::new();
    for id in [profile_id, nand_id] {
        loop {
            if let Ok(guard) = ExclusiveResourceKey::Resource(id).acquire() {
                guards.push(guard);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "save watcher did not finish after repair"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    let reopened = manager
        .open(StorageKind::SaveRepo, WiiNand::PLATFORM, profile_id)
        .await
        .unwrap();
    let saves = reopened.save_repo().unwrap();
    assert!(!saves.checkpoint_saves().await.unwrap());
    let mut bytes = [0; 8];
    assert_eq!(
        saves
            .open(save_path, false)
            .unwrap()
            .read_at(0, &mut bytes)
            .unwrap(),
        8
    );
    assert_eq!(&bytes, b"updated!");
}
