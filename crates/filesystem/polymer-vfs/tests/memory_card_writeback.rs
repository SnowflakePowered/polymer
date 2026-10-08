use std::io::Read;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use polymer_consolestorage::impls::ps1::PlaystationMemoryCard;
use polymer_consolestorage::{
    GitVolumeStorage, MemoryCard, MemoryCardView, PlatformRepository, Storage, StorageKind,
    VersionedStorage,
};
use polymer_vfs::{
    ExclusiveResourceKey, FilesystemPlan, FinalizationReason, MemoryCardFileFactory, Permissions,
    Resource, ResourceOrigin, VfsPathBuf,
};
use uuid::Uuid;

const SAVE_PATH: &str = "card/BASLUS-00000";
const PAYLOAD_OFFSET: usize = 1024;

struct CardFixture {
    card: PlaystationMemoryCard,
    volume: GitVolumeStorage,
    view: MemoryCardView,
    offset: u64,
    _directory: tempfile::TempDir,
}

impl CardFixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let platform =
            PlatformRepository::open(directory.path().join("platform"), "SONY_PSX").unwrap();
        let id = Uuid::new_v4();
        let volume = platform.volume(StorageKind::MemoryCard, id);
        let card = PlaystationMemoryCard::new_empty(
            VersionedStorage::new(volume.clone()).unwrap(),
            id,
            None,
        )
        .await
        .unwrap();
        let mut payload = vec![0; 8192];
        payload[..4].copy_from_slice(&[b'S', b'C', 0x11, 1]);
        payload[PAYLOAD_OFFSET..PAYLOAD_OFFSET + 32].fill(37);
        card.install_bytes(&payload, SAVE_PATH).unwrap();
        let image = card.raw_image(&[]).unwrap();
        let offset = image
            .windows(32)
            .position(|bytes| bytes == [37; 32])
            .unwrap() as u64;
        let view = card.raw_view(&[]).unwrap();
        Self {
            card,
            volume,
            view,
            offset,
            _directory: directory,
        }
    }

    fn obstruct(&self) {
        let root = self.volume.checkout_path();
        std::fs::rename(root.join("card"), root.join("blocked-card")).unwrap();
        std::fs::write(root.join("card"), b"not a directory").unwrap();
    }

    fn repair(&self) {
        let root = self.volume.checkout_path();
        std::fs::remove_file(root.join("card")).unwrap();
        std::fs::create_dir_all(root.join("card")).unwrap();
    }
}

#[tokio::test]
async fn sync_replaces_the_save_without_truncating_existing_readers() {
    let fixture = CardFixture::new().await;
    let mut previous = fixture.card.open_read(SAVE_PATH).unwrap();
    let file = fixture.view.storage().open("", true).unwrap();
    file.write_all_at(fixture.offset, &[91; 32]).unwrap();
    file.sync().unwrap();
    let current = std::fs::read(fixture.volume.checkout_path().join(SAVE_PATH)).unwrap();
    assert_eq!(&current[PAYLOAD_OFFSET..PAYLOAD_OFFSET + 32], &[91; 32]);
    let mut old = Vec::new();
    previous.read_to_end(&mut old).unwrap();
    assert_eq!(&old[PAYLOAD_OFFSET..PAYLOAD_OFFSET + 32], &[37; 32]);
    fixture.view.storage().finish("").unwrap();
}

#[tokio::test]
async fn concurrent_syncs_persist_acknowledged_writes() {
    let fixture = CardFixture::new().await;
    let barrier = Arc::new(Barrier::new(2));
    std::thread::scope(|threads| {
        for index in 0..2 {
            let barrier = barrier.clone();
            let fixture = &fixture;
            threads.spawn(move || {
                let file = fixture.view.storage().open("", true).unwrap();
                let mut observed = Vec::new();
                for round in 1..=32u8 {
                    file.write_all_at(fixture.offset + index as u64, &[round])
                        .unwrap();
                    barrier.wait();
                    file.sync().unwrap();
                    let current =
                        std::fs::read(fixture.volume.checkout_path().join(SAVE_PATH)).unwrap();
                    observed.push((round, current[PAYLOAD_OFFSET..PAYLOAD_OFFSET + 2].to_vec()));
                    barrier.wait();
                }
                for (round, bytes) in observed {
                    assert_eq!(bytes, [round; 2]);
                }
            });
        }
    });
    fixture.view.storage().finish("").unwrap();
}

#[tokio::test]
async fn failed_shutdown_keeps_the_image_and_can_be_retried() {
    let fixture = CardFixture::new().await;
    let file = fixture.view.storage().open("", true).unwrap();
    fixture.obstruct();
    file.write_all_at(fixture.offset, &[91; 32]).unwrap();
    assert!(file.sync().is_err());
    assert!(fixture.view.storage().finish("").is_err());
    assert!(fixture.view.storage().finish("").is_err());
    assert!(file.write_at(fixture.offset, &[12]).is_err());
    let previous = std::fs::read(
        fixture
            .volume
            .checkout_path()
            .join("blocked-card/BASLUS-00000"),
    )
    .unwrap();
    assert_eq!(&previous[PAYLOAD_OFFSET..PAYLOAD_OFFSET + 32], &[37; 32]);
    fixture.repair();
    fixture.view.storage().finish("").unwrap();
    let current = std::fs::read(fixture.volume.checkout_path().join(SAVE_PATH)).unwrap();
    assert_eq!(&current[PAYLOAD_OFFSET..PAYLOAD_OFFSET + 32], &[91; 32]);
}

#[tokio::test]
async fn dropping_failed_finalization_retains_exclusivity_until_writeback_succeeds() {
    let fixture = CardFixture::new().await;
    let resource_id = Uuid::new_v4();
    let mut plan = FilesystemPlan::default();
    plan.mount_file(
        VfsPathBuf::new("/card").unwrap(),
        Arc::new(Resource::new(
            Arc::new(MemoryCardFileFactory::new(fixture.view.clone(), resource_id).unwrap()),
            Permissions::ALL,
            ResourceOrigin::Scratch,
        )),
        Permissions::ALL,
    );
    let filesystem = plan.seal().unwrap();
    filesystem.materialize().unwrap();
    fixture.obstruct();
    let file = fixture.view.storage().open("", true).unwrap();
    file.write_all_at(fixture.offset, &[91; 32]).unwrap();
    assert!(
        filesystem
            .finalize(FinalizationReason::ProcessExited)
            .is_err()
    );
    drop(filesystem);
    let key = ExclusiveResourceKey::Resource(resource_id);
    assert!(key.clone().acquire().is_err());
    fixture.repair();
    let deadline = Instant::now() + Duration::from_secs(10);
    let acquired = loop {
        if let Ok(lease) = key.clone().acquire() {
            break lease;
        }
        assert!(
            Instant::now() < deadline,
            "finalization did not recover after repairing storage"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    let current = std::fs::read(fixture.volume.checkout_path().join(SAVE_PATH)).unwrap();
    assert_eq!(&current[PAYLOAD_OFFSET..PAYLOAD_OFFSET + 32], &[91; 32]);
    drop(acquired);
}

#[tokio::test]
async fn shutdown_drains_writes_that_race_with_closing() {
    let fixture = CardFixture::new().await;
    let barrier = Arc::new(Barrier::new(3));
    let acknowledged = std::thread::scope(|threads| {
        let writers = (0..2)
            .map(|index| {
                let barrier = barrier.clone();
                let fixture = &fixture;
                threads.spawn(move || {
                    let file = fixture.view.storage().open("", true).unwrap();
                    file.write_all_at(fixture.offset + index as u64, &[1])
                        .unwrap();
                    let mut acknowledged = 1;
                    barrier.wait();
                    for value in 2..=128u8 {
                        if file
                            .write_at(fixture.offset + index as u64, &[value])
                            .is_err()
                        {
                            break;
                        }
                        acknowledged = value;
                    }
                    acknowledged
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        fixture.view.storage().finish("").unwrap();
        writers
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>()
    });
    let current = std::fs::read(fixture.volume.checkout_path().join(SAVE_PATH)).unwrap();
    assert_eq!(&current[PAYLOAD_OFFSET..PAYLOAD_OFFSET + 2], acknowledged);
}
