use std::{io::Read, sync::Arc, time::Duration};

use polymer_consolestorage::{
    MemoryCard, Storage, StorageKind,
    impls::{
        gamecube::{GameCubeMemoryCard, gci},
        ps2::Playstation2MemoryCard,
    },
};
use polymer_library::StorageManager;
use polymer_vfs::{MemoryFileFactory, OpenOptions, ResourceFactory, VfsError};

struct GameCubeFixture {
    _directory: tempfile::TempDir,
    card: Arc<GameCubeMemoryCard>,
    save: gci::Save<'static>,
}

impl GameCubeFixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let card = StorageManager::new(directory.path())
            .create(StorageKind::MemoryCard, GameCubeMemoryCard::PLATFORM, None)
            .await
            .unwrap()
            .downcast::<GameCubeMemoryCard>()
            .unwrap();
        let mut bytes = vec![0; gci::DIRENT_SIZE + gci::BLOCK_SIZE];
        bytes[..6].copy_from_slice(b"GALE01");
        bytes[8..12].copy_from_slice(b"save");
        bytes[0x38..0x3a].copy_from_slice(&1u16.to_be_bytes());
        bytes[gci::DIRENT_SIZE..][..32].copy_from_slice(b"original GameCube save payload!!");
        let save = gci::Save::new(bytes).unwrap();
        card.create(&format!("card/{}", save.file_name()))
            .unwrap()
            .write_all_at(0, save.as_bytes())
            .unwrap();
        Self {
            _directory: directory,
            card,
            save,
        }
    }

    fn saved_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        self.card
            .open_read(&format!("card/{}", self.save.file_name()))
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }
}

#[tokio::test]
async fn raw_card_sync_retries_bad_images_and_open_handles_keep_writeback_alive() {
    let fixture = GameCubeFixture::new().await;
    let view = fixture
        .card
        .view(&[fixture.save.depot_id()], "raw")
        .unwrap();
    let file = view.storage().open("", true).unwrap();
    let mut image = Vec::new();
    view.storage()
        .open_read("")
        .unwrap()
        .read_to_end(&mut image)
        .unwrap();
    let offset = image
        .windows(32)
        .position(|bytes| bytes == &fixture.save.data()[..32])
        .unwrap();

    // A malformed intermediate image must not destroy the canonical save.
    file.set_len(16).unwrap();
    assert!(file.sync().is_err());
    assert_eq!(fixture.saved_bytes(), fixture.save.as_bytes());
    file.write_all_at(0, &image).unwrap();
    file.write_all_at(offset as u64, b"updated").unwrap();
    file.sync().unwrap();
    assert_eq!(&fixture.saved_bytes()[gci::DIRENT_SIZE..][..7], b"updated");

    drop(view);
    file.write_all_at(offset as u64, b"survives").unwrap();
    file.sync().unwrap();
    assert_eq!(&fixture.saved_bytes()[gci::DIRENT_SIZE..][..8], b"survives");
    file.write_all_at(offset as u64, b"lastdrop").unwrap();
    drop(file);
    tokio::time::timeout(Duration::from_secs(5), async {
        while &fixture.saved_bytes()[gci::DIRENT_SIZE..][..8] != b"lastdrop" {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn dolphin_folder_writes_reconcile_in_background_and_tombstones_delete_saves() {
    let fixture = GameCubeFixture::new().await;
    let view = fixture
        .card
        .view(&[fixture.save.depot_id()], "dolphin")
        .unwrap();
    let path = fixture.save.file_name();
    let file = view.storage().open(&path, true).unwrap();
    file.write_all_at(gci::DIRENT_SIZE as u64, b"background")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while &fixture.saved_bytes()[gci::DIRENT_SIZE..][..10] != b"background" {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    // Replacing the directory entry leaves the old handle detached from the
    // projection that reconciliation persists.
    let replacement = view.storage().create(&path).unwrap();
    replacement
        .write_all_at(0, fixture.save.as_bytes())
        .unwrap();
    replacement
        .write_all_at(gci::DIRENT_SIZE as u64, b"replacement")
        .unwrap();
    file.write_all_at(gci::DIRENT_SIZE as u64, b"old handle")
        .unwrap();
    file.sync().unwrap();
    assert_eq!(
        &fixture.saved_bytes()[gci::DIRENT_SIZE..][..11],
        b"replacement"
    );
    view.storage()
        .rename(&path, &format!("{path}.deleted"))
        .unwrap();
    assert!(!view.storage().exists(&path));
    assert!(!view.storage().exists(&format!("{path}.deleted")));
    let mut retained = [0; 10];
    file.read_at(gci::DIRENT_SIZE as u64, &mut retained)
        .unwrap();
    assert_eq!(&retained, b"old handle");
    view.storage().finish("").unwrap();
    assert!(!fixture.card.exists(&format!("card/{path}")));
}

#[tokio::test]
async fn pcsx2_folder_edits_roundtrip_through_psu_and_delete_at_finish() {
    let directory = tempfile::tempdir().unwrap();
    let card = StorageManager::new(directory.path())
        .create(
            StorageKind::MemoryCard,
            Playstation2MemoryCard::PLATFORM,
            None,
        )
        .await
        .unwrap()
        .downcast::<Playstation2MemoryCard>()
        .unwrap();
    let view = card.view(&[], "pcsx2").unwrap();
    let root = "BASLUS-12345SAVE";
    let path = format!("{root}/data");
    view.storage().create_dir_all(root).unwrap();
    view.storage()
        .install_file(Box::new(std::io::Cursor::new(b"first payload")), &path)
        .await
        .unwrap();
    let old = view.storage().open(&path, true).unwrap();
    view.storage()
        .create(&path)
        .unwrap()
        .write_all_at(0, b"first payload")
        .unwrap();
    old.write_all_at(0, b"stale").unwrap();
    let mut retained = [0; 5];
    old.read_at(0, &mut retained).unwrap();
    assert_eq!(&retained, b"stale");
    view.storage()
        .rename(&path, &format!("{root}/renamed"))
        .unwrap();
    let path = format!("{root}/renamed");
    view.storage().truncate(&path, 5).unwrap();
    view.storage()
        .open(&path, true)
        .unwrap()
        .write_all_at(5, b" revision")
        .unwrap();
    view.storage().sync("").unwrap();

    let reopened = card.view(&[], "pcsx2").unwrap();
    let mut bytes = Vec::new();
    reopened
        .storage()
        .open_read(&path)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, b"first revision");
    reopened.storage().finish("").unwrap();
    view.storage()
        .rename(root, &format!("_pcsx2_deleted_{root}"))
        .unwrap();
    assert!(!view.storage().exists(root));
    assert!(!view.storage().exists(&path));
    assert!(!view.storage().exists(&format!("_pcsx2_deleted_{root}")));
    assert!(
        !view
            .storage()
            .exists(&format!("_pcsx2_deleted_{root}/renamed"))
    );
    view.storage().finish("").unwrap();
    assert!(!card.exists(&format!("card/{root}.psu")));
}

#[tokio::test]
async fn card_reconciliation_retries_failed_persistence_on_sync() {
    let fixture = GameCubeFixture::new().await;
    let view = fixture
        .card
        .view(&[fixture.save.depot_id()], "dolphin")
        .unwrap();
    let path = fixture.save.file_name();
    let destination = format!("card/{path}");
    let file = view.storage().open(&path, true).unwrap();

    // An obstructing directory makes persistence fail through the real backing storage.
    fixture.card.remove(&destination).unwrap();
    fixture.card.create_dir_all(&destination).unwrap();
    file.write_all_at(gci::DIRENT_SIZE as u64, b"recovered")
        .unwrap();
    assert!(file.sync().is_err());
    fixture.card.remove(&destination).unwrap();
    // No new write is needed: failed reconciliation stays dirty.
    file.sync().unwrap();
    assert_eq!(
        &fixture.saved_bytes()[gci::DIRENT_SIZE..][..9],
        b"recovered"
    );
    view.storage().finish("").unwrap();
}

#[test]
fn memory_files_keep_open_access_separate_from_shared_contents() {
    let standalone = MemoryFileFactory::writable(Vec::new())
        .materialize()
        .unwrap();
    let read = OpenOptions {
        read: true,
        write: false,
        truncate: false,
    };

    let file = standalone.backend;
    let reader = file.open(read).unwrap();
    let writer = file
        .open(OpenOptions {
            write: true,
            ..read
        })
        .unwrap();
    writer.write(2, b"data").unwrap();
    assert!(!reader.metadata().unwrap().readonly);
    assert!(matches!(
        reader.write(0, b"x"),
        Err(VfsError::PermissionDenied(_))
    ));
    assert!(matches!(
        reader.truncate(0),
        Err(VfsError::PermissionDenied(_))
    ));
    let mut bytes = [0xff; 6];
    assert_eq!(reader.read(0, &mut bytes).unwrap(), bytes.len());
    assert_eq!(&bytes, b"\0\0data");

    writer.truncate(4).unwrap();
    assert_eq!(reader.metadata().unwrap().len, 4);
    // Open-time truncation does not grant write access to the returned handle.
    let truncated = file
        .open(OpenOptions {
            truncate: true,
            ..read
        })
        .unwrap();
    assert_eq!(reader.metadata().unwrap().len, 0);
    assert!(matches!(
        truncated.write(0, b"x"),
        Err(VfsError::PermissionDenied(_))
    ));
    assert!(matches!(
        truncated.truncate(1),
        Err(VfsError::PermissionDenied(_))
    ));
    writer.write(0, b"new").unwrap();
    assert_eq!(truncated.read(0, &mut bytes).unwrap(), 3);
    assert_eq!(&bytes[..3], b"new");
}

#[test]
fn memory_file_factory_shares_storage_across_handles_and_adoption() {
    let factory = MemoryFileFactory::writable(b"before".to_vec());
    let first = factory.materialize().unwrap();
    let second = factory.materialize().unwrap();
    let read = OpenOptions {
        read: true,
        write: false,
        truncate: false,
    };
    let reader = first.backend.open(read).unwrap();
    let writer = first
        .backend
        .open(OpenOptions {
            write: true,
            ..read
        })
        .unwrap();
    assert!(!reader.metadata().unwrap().readonly);
    let source = MemoryFileFactory::immutable(b"replacement".to_vec())
        .materialize()
        .unwrap();
    let adoption = second.backend.adopt(source.backend.clone()).unwrap();
    adoption.write(0, b"R").unwrap();
    let mut bytes = [0; 11];
    reader.read(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"Replacement");
    assert!(reader.write(0, b"x").is_err());
    adoption.truncate(4).unwrap();
    assert_eq!(reader.metadata().unwrap().len, 4);
    adoption.finalize().unwrap();
    reader.read(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"replacement");
    writer.write(0, b"R").unwrap();
    second
        .backend
        .open(read)
        .unwrap()
        .read(0, &mut bytes)
        .unwrap();
    assert_eq!(&bytes, b"Replacement");
    assert!(
        source
            .backend
            .open(OpenOptions {
                write: true,
                ..read
            })
            .is_err()
    );
}

#[test]
fn memory_adoption_publishes_complete_replacements_to_existing_readers() {
    let target = MemoryFileFactory::writable(vec![0x11; 1024 * 1024])
        .materialize()
        .unwrap();
    let sources = [0x22, 0x33].map(|byte| {
        MemoryFileFactory::immutable(vec![byte; 1024 * 1024])
            .materialize()
            .unwrap()
    });
    let reader = target
        .backend
        .open(OpenOptions {
            read: true,
            write: false,
            truncate: false,
        })
        .unwrap();
    let start = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            start.wait();
            for index in 0..64 {
                target
                    .backend
                    .adopt(sources[index % 2].backend.clone())
                    .unwrap();
            }
        });
        start.wait();
        let mut bytes = vec![0; 1024 * 1024];
        for _ in 0..512 {
            assert_eq!(reader.read(0, &mut bytes).unwrap(), bytes.len());
            assert!(matches!(bytes[0], 0x11 | 0x22 | 0x33));
            assert!(
                bytes.iter().all(|byte| *byte == bytes[0]),
                "a partial replacement became visible"
            );
        }
    });
}

#[cfg(windows)]
#[test]
fn failed_memory_adoption_and_finalization_leave_the_target_unchanged() {
    // Windows byte-range locks make actual source reads fail, after metadata and open succeed.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.bin");
    std::fs::write(&path, vec![0x22; 128 * 1024]).unwrap();
    let source = polymer_vfs::HostFileFactory::new(path.clone(), false)
        .materialize()
        .unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let target = MemoryFileFactory::writable(vec![0x11; 64 * 1024])
        .materialize()
        .unwrap();
    let read = OpenOptions {
        read: true,
        write: false,
        truncate: false,
    };
    let reader = target.backend.open(read).unwrap();

    lock.lock().unwrap();
    assert!(
        source
            .backend
            .open(read)
            .unwrap()
            .read(0, &mut [0; 1])
            .is_err()
    );
    assert!(target.backend.adopt(source.backend.clone()).is_err());
    let mut bytes = vec![0; 64 * 1024];
    assert_eq!(reader.metadata().unwrap().len, bytes.len() as u64);
    assert_eq!(reader.read(0, &mut bytes).unwrap(), bytes.len());
    assert!(bytes.iter().all(|byte| *byte == 0x11));
    lock.unlock().unwrap();

    let adoption = target.backend.adopt(source.backend.clone()).unwrap();
    std::fs::write(&path, vec![0x33; 256 * 1024]).unwrap();
    lock.lock().unwrap();
    assert!(adoption.finalize().is_err());
    bytes.resize(128 * 1024, 0);
    assert_eq!(reader.metadata().unwrap().len, bytes.len() as u64);
    assert_eq!(reader.read(0, &mut bytes).unwrap(), bytes.len());
    assert!(bytes.iter().all(|byte| *byte == 0x22));
    lock.unlock().unwrap();

    adoption.finalize().unwrap();
    bytes.resize(256 * 1024, 0);
    assert_eq!(reader.read(0, &mut bytes).unwrap(), bytes.len());
    assert!(bytes.iter().all(|byte| *byte == 0x33));
}
