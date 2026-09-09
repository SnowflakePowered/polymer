use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use uuid::Uuid;

use super::PspMemoryStick;
use crate::savegame::{Package, SaveBlob};
use crate::{ConsoleStorage, PlatformRepository, Save, SaveRepo, StorageKind};

struct Fixture {
    _directory: tempfile::TempDir,
    platform: PlatformRepository,
    nand: crate::Nand,
    saves: Arc<dyn SaveRepo>,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let platform = PlatformRepository::open(
            directory.path().join("SONY_PSP.git"),
            PspMemoryStick::PLATFORM,
        )
        .unwrap();
        let nand_id = Uuid::now_v7();
        let ConsoleStorage::Nand(nand) = ConsoleStorage::create(
            StorageKind::Nand,
            PspMemoryStick::PLATFORM,
            platform.volume(StorageKind::Nand, nand_id),
            nand_id,
            None,
        )
        .await
        .unwrap() else {
            panic!("expected NAND")
        };
        let save_id = Uuid::now_v7();
        let ConsoleStorage::SaveRepo(saves) = ConsoleStorage::create(
            StorageKind::SaveRepo,
            PspMemoryStick::PLATFORM,
            platform.volume(StorageKind::SaveRepo, save_id),
            save_id,
            None,
        )
        .await
        .unwrap() else {
            panic!("expected save repository")
        };
        Self {
            _directory: directory,
            platform,
            nand,
            saves,
        }
    }

    async fn write(&self, path: &str, bytes: &[u8]) {
        self.saves
            .install_file(Box::new(Cursor::new(bytes.to_vec())), path)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn native_composition_exports_and_restores_all_slots_as_one_family() {
    let fixture = Fixture::new().await;
    let saves = &fixture.saves;
    let package = Package::new(Some("ulus-12345".into()), Some("NPUH99999".into()));
    let id = saves.infer_save_depot(&package);
    let composed = fixture.nand.compose(saves.clone()).unwrap();
    for slot in ["SLOT1", "SLOT2", "SYSTEM"] {
        let directory = format!("PSP/SAVEDATA/ULUS12345{slot}");
        composed.create_dir_all(&directory).unwrap();
        let file = composed.create(&format!("{directory}/DATA.BIN")).unwrap();
        file.write_all_at(0, slot.as_bytes()).unwrap();
    }
    fixture
        .write("PSP/SAVEDATA/ULES12345SLOT1/DATA.BIN", b"other region")
        .await;
    // Malformed metadata must not make a family disappear.
    fixture
        .write("PSP/SAVEDATA/ULUS12345SLOT1/PARAM.SFO", b"invalid")
        .await;
    assert!(saves.exists("PSP/SAVEDATA/ULUS12345SLOT2/DATA.BIN"));
    assert!(
        !fixture
            .nand
            .root()
            .exists("PSP/SAVEDATA/ULUS12345SLOT2/DATA.BIN")
    );
    assert!(!saves.claims("PSP/GAME/title/EBOOT.PBP"));
    assert!(!saves.claims("PSP/SAVEDATA_OTHER/file"));
    assert_eq!(saves.enumerate_saves().count(), 2);
    let exported = saves.export_save(&id).unwrap();
    assert_eq!(exported.blobs.len(), 4);
    assert!(
        exported
            .blobs
            .iter()
            .all(|blob| blob.target_path.starts_with("PSP/SAVEDATA"))
    );
    let save = Save::new(saves.as_ref(), id);
    let original = save.checkpoint().await.unwrap().unwrap();
    fixture
        .write("PSP/SAVEDATA/ULUS12345SLOT3/DATA.BIN", b"new slot")
        .await;
    fixture
        .write("PSP/SAVEDATA/ULUS12345SLOT1/DATA.BIN", b"changed")
        .await;
    save.restore(original.id).await.unwrap();
    assert!(!saves.exists("PSP/SAVEDATA/ULUS12345SLOT3/DATA.BIN"));
    assert!(saves.exists("PSP/SAVEDATA/ULES12345SLOT1/DATA.BIN"));
    let restored = saves.export_save(&id).unwrap();
    assert_eq!(restored.blobs.len(), 4);
    assert_eq!(
        restored
            .blobs
            .iter()
            .find(|blob| blob.target_path.ends_with("ULUS12345SLOT1/DATA.BIN"))
            .unwrap()
            .bytes,
        b"SLOT1"
    );

    let destination = Fixture::new().await;
    save.copy_to(destination.saves.as_ref()).await.unwrap();
    assert_eq!(destination.saves.export_save(&id).unwrap().blobs.len(), 4);
    let volume_id = saves.volumeinfo().await.unwrap().unwrap().id;
    let ConsoleStorage::SaveRepo(reopened) = ConsoleStorage::open(
        StorageKind::SaveRepo,
        fixture.platform.volume(StorageKind::SaveRepo, volume_id),
    )
    .await
    .unwrap() else {
        panic!("expected save repository")
    };
    assert_eq!(reopened.export_save(&id).unwrap().blobs.len(), 4);
}

#[tokio::test]
async fn invalid_imports_do_not_modify_live_family() {
    let fixture = Fixture::new().await;
    let path = "PSP/SAVEDATA/ULUS12345SLOT1/DATA.BIN";
    fixture.write(path, b"original").await;
    let id = fixture
        .saves
        .infer_save_depot(&Package::new(Some("ULUS12345".into()), None));
    for invalid in [
        "PSP/SAVEDATA/ULES12345SLOT2/DATA.BIN",
        "PSP/GAME/ULUS12345/EBOOT.PBP",
        "PSP/SAVEDATA/ULUS12345SLOT1/../escape",
    ] {
        let blobs = [path, invalid].map(|path| SaveBlob {
            target_path: PathBuf::from(path),
            bytes: b"replacement".to_vec(),
            blake3: *blake3::hash(b"replacement").as_bytes(),
        });
        assert!(
            fixture
                .saves
                .install_save(Box::pin(futures::stream::iter(blobs)))
                .await
                .is_err()
        );
        assert_eq!(
            fixture.saves.export_save(&id).unwrap().blobs[0].bytes,
            b"original"
        );
    }
}
