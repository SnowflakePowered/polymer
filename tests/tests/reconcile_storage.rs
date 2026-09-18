use std::{io::Read, sync::Arc, time::Duration};

use polymer_consolestorage::{
    MemoryCard, Storage, StorageKind,
    impls::{
        gamecube::{GameCubeMemoryCard, gci},
        ps2::Playstation2MemoryCard,
    },
};
use polymer_library::StorageManager;
use polymer_vfs::{MemoryFileFactory, OpenOptions, ResourceFactory};

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
    view.storage()
        .rename(&path, &format!("{path}.deleted"))
        .unwrap();
    assert!(!view.storage().exists(&path));
    assert!(!view.storage().exists(&format!("{path}.deleted")));
    let mut retained = [0; 10];
    file.read_at(gci::DIRENT_SIZE as u64, &mut retained)
        .unwrap();
    assert_eq!(&retained, b"background");
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
    let source = MemoryFileFactory::immutable(b"replacement".to_vec())
        .materialize()
        .unwrap();
    let adoption = second.backend.adopt(source.backend.clone()).unwrap();
    adoption.write(0, b"R").unwrap();
    let mut bytes = [0; 11];
    reader.read(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"Replacement");
    assert!(reader.write(0, b"x").is_err());
    adoption.finalize().unwrap();
    reader.read(0, &mut bytes).unwrap();
    assert_eq!(&bytes, b"replacement");
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
