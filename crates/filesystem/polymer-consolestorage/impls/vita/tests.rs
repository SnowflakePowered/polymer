use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use uuid::Uuid;

use super::VitaNand;
use crate::savegame::{Package, SaveBlob};
use crate::{ConsoleStorage, PlatformRepository, Save, SaveRepo, StorageKind};

struct Fixture {
    _directory: tempfile::TempDir,
    platform: PlatformRepository,
    nand: crate::Nand,
    saves: Arc<dyn SaveRepo>,
}

impl Fixture {
    fn sfo(values: &[(&str, &str)]) -> Vec<u8> {
        let mut keys = Vec::new();
        let mut data = Vec::new();
        let mut entries = Vec::new();
        for (key, value) in values {
            entries.extend_from_slice(&(keys.len() as u16).to_le_bytes());
            entries.extend_from_slice(&0x0204_u16.to_le_bytes());
            entries.extend_from_slice(&((value.len() + 1) as u32).to_le_bytes());
            entries.extend_from_slice(&((value.len() + 1) as u32).to_le_bytes());
            entries.extend_from_slice(&(data.len() as u32).to_le_bytes());
            keys.extend_from_slice(key.as_bytes());
            keys.push(0);
            data.extend_from_slice(value.as_bytes());
            data.push(0);
        }
        let key_offset = 20 + entries.len();
        let data_offset = key_offset + keys.len();
        let mut bytes = b"\0PSF".to_vec();
        bytes.extend_from_slice(&0x101_u32.to_le_bytes());
        bytes.extend_from_slice(&(key_offset as u32).to_le_bytes());
        bytes.extend_from_slice(&(data_offset as u32).to_le_bytes());
        bytes.extend_from_slice(&(values.len() as u32).to_le_bytes());
        bytes.extend(entries);
        bytes.extend(keys);
        bytes.extend(data);
        bytes
    }

    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let platform =
            PlatformRepository::open(directory.path().join("SONY_PSV.git"), VitaNand::PLATFORM)
                .unwrap();
        let nand_id = Uuid::now_v7();
        let ConsoleStorage::Nand(nand) = ConsoleStorage::create(
            StorageKind::Nand,
            VitaNand::PLATFORM,
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
            VitaNand::PLATFORM,
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
async fn native_metadata_and_profile_switching_keep_the_same_guest_user() {
    use crate::Permissions;
    use crate::nand::NandCompletion;

    let fixture = Fixture::new().await;
    let other = Fixture::new().await;
    let directory = "ux0/user/00/savedata/PCSE12345";
    fixture
        .write(
            &format!("{directory}/sce_sys/param.sfo"),
            &Fixture::sfo(&[("TITLE", "Vita save"), ("SUB_TITLE", "Chapter two")]),
        )
        .await;
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(2, 2)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    fixture
        .write(&format!("{directory}/sce_sys/icon0.png"), png.get_ref())
        .await;
    fixture
        .write("ux0/user/00/trophy/profile.bin", b"trophy state")
        .await;
    let id = fixture
        .saves
        .infer_save_depot(&Package::new(None, Some("PCSE12345".into())));
    let descriptor = fixture.saves.enumerate_saves().next().unwrap();
    assert_eq!(descriptor.title, "Vita save");
    assert_eq!(descriptor.description, "Chapter two");
    assert!(fixture.saves.read_save_icon(id).unwrap().is_some());
    assert_eq!(fixture.saves.export_save(&id).unwrap().blobs.len(), 2);
    let composed = fixture.nand.compose(other.saves.clone()).unwrap();
    assert!(!composed.exists(&format!("{directory}/sce_sys/param.sfo")));
    assert!(composed.exists(super::GUEST_SAVE_ROOT));
    assert!(!composed.exists("ux0/user/01"));
    assert!(!fixture.nand.is_complete(NandCompletion::Minimal));
    assert!(
        !fixture
            .nand
            .permissions("ux0/app/PCSE12345/eboot.bin")
            .contains(Permissions::WRITE)
    );
    assert!(
        fixture
            .nand
            .permissions(&format!("{directory}/data.bin"))
            .contains(Permissions::WRITE)
    );
    assert!(
        !fixture
            .nand
            .permissions("ux0/user/01/savedata/file")
            .contains(Permissions::WRITE)
    );

    // A partially published save cannot be exported as a complete snapshot.
    fixture
        .saves
        .create_dir_all("ux0/user/00/savedata/.pending")
        .unwrap();
    assert!(matches!(
        fixture.saves.export_save(&id),
        Err(crate::Error::HistoryConflict(_))
    ));
}

#[tokio::test]
async fn directory_depots_route_content_and_uninstall_without_touching_saves() {
    use super::VitaDirectoryDepot;
    use crate::installable::{Depot, DepotType};
    use crate::nand::VirtualNand;

    let fixture = Fixture::new().await;
    let nand = VitaNand::open(
        fixture
            .platform
            .volume(StorageKind::Nand, fixture.nand.id().await.unwrap()),
    )
    .await
    .unwrap();
    let source = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(source.path().join("sce_sys")).unwrap();
    std::fs::write(source.path().join("eboot.bin"), b"decrypted executable").unwrap();
    fixture
        .write("ux0/user/00/savedata/PCSE12345/data.bin", b"keep")
        .await;
    let mut artifacts = Vec::new();
    for (category, kind, path) in [
        ("gd", DepotType::App, "ux0/app/PCSE12345"),
        ("gp", DepotType::AppUpdate, "ux0/patch/PCSE12345"),
        (
            "ac",
            DepotType::AppContent,
            "ux0/addcont/PCSE12345/EXTRACONTENT0001",
        ),
    ] {
        std::fs::write(
            source.path().join("sce_sys/param.sfo"),
            Fixture::sfo(&[
                ("TITLE_ID", "PCSE12345"),
                ("CATEGORY", category),
                ("TITLE", "Vita game"),
                ("CONTENT_ID", "UP0000-PCSE12345_00-EXTRACONTENT0001"),
                ("APP_VER", "01.00"),
            ]),
        )
        .unwrap();
        let depot = VitaDirectoryDepot::open(source.path()).unwrap();
        assert_eq!(depot.install_path(), path);
        let artifact = depot.install(&nand).finish().await.unwrap();
        assert_eq!(artifact.depot_type(), kind);
        assert_eq!(artifact.label(), "Vita game");
        assert_eq!(artifact.version(), Some("01.00"));
        assert_eq!(artifact.manifest().len(), 2);
        assert!(
            artifact
                .manifest()
                .iter()
                .all(|file| file.path().starts_with(path))
        );
        nand.record(&artifact).await.unwrap();
        artifacts.push(artifact);
    }
    assert_eq!(nand.artifacts().await.unwrap().len(), 3);
    nand.uninstall(&[artifacts[1].id()]).await.unwrap();
    assert!(nand.root().exists("ux0/app/PCSE12345/eboot.bin"));
    assert!(!nand.root().exists("ux0/patch/PCSE12345/eboot.bin"));
    assert!(
        fixture
            .saves
            .exists("ux0/user/00/savedata/PCSE12345/data.bin")
    );

    // Malformed content IDs must fail before they can become install paths.
    std::fs::write(
        source.path().join("sce_sys/param.sfo"),
        Fixture::sfo(&[
            ("TITLE_ID", "PCSE12345"),
            ("CATEGORY", "ac"),
            ("CONTENT_ID", "../../escape"),
        ]),
    )
    .unwrap();
    assert!(VitaDirectoryDepot::open(source.path()).is_err());
}

#[tokio::test]
async fn native_composition_exports_and_restores_all_slots_as_one_family() {
    let fixture = Fixture::new().await;
    let saves = &fixture.saves;
    let package = Package::new(Some("PCSB99999".into()), Some("PCSE12345".into()));
    let id = saves.infer_save_depot(&package);
    let composed = fixture.nand.compose(saves.clone()).unwrap();
    for slot in ["SLOT1", "SLOT2", "SYSTEM"] {
        let directory = format!("ux0/user/00/savedata/PCSE12345/{slot}");
        composed.create_dir_all(&directory).unwrap();
        let file = composed.create(&format!("{directory}/DATA.BIN")).unwrap();
        file.write_all_at(0, slot.as_bytes()).unwrap();
    }
    fixture
        .write(
            "ux0/user/00/savedata/PCSB12345/SLOT1/DATA.BIN",
            b"other region",
        )
        .await;
    // Malformed metadata must not make a family disappear.
    fixture
        .write(
            "ux0/user/00/savedata/PCSE12345/SLOT1/sce_sys/param.sfo",
            b"invalid",
        )
        .await;
    assert!(saves.exists("ux0/user/00/savedata/PCSE12345/SLOT2/DATA.BIN"));
    assert!(
        !fixture
            .nand
            .root()
            .exists("ux0/user/00/savedata/PCSE12345/SLOT2/DATA.BIN")
    );
    assert!(!saves.claims("ux0/app/title/eboot.bin"));
    assert!(!saves.claims("ux0/user/01/savedata/PCSE12345/file"));
    assert_eq!(saves.enumerate_saves().count(), 2);
    let exported = saves.export_save(&id).unwrap();
    assert_eq!(exported.blobs.len(), 4);
    assert!(
        exported
            .blobs
            .iter()
            .all(|blob| blob.target_path.starts_with("ux0/user/00/savedata"))
    );
    let save = Save::new(saves.as_ref(), id);
    let original = save.checkpoint().await.unwrap().unwrap();
    fixture
        .write("ux0/user/00/savedata/PCSE12345/SLOT3/DATA.BIN", b"new slot")
        .await;
    fixture
        .write("ux0/user/00/savedata/PCSE12345/SLOT1/DATA.BIN", b"changed")
        .await;
    save.restore(original.id).await.unwrap();
    assert!(!saves.exists("ux0/user/00/savedata/PCSE12345/SLOT3/DATA.BIN"));
    assert!(saves.exists("ux0/user/00/savedata/PCSB12345/SLOT1/DATA.BIN"));
    let restored = saves.export_save(&id).unwrap();
    assert_eq!(restored.blobs.len(), 4);
    assert_eq!(
        restored
            .blobs
            .iter()
            .find(|blob| blob.target_path.ends_with("PCSE12345/SLOT1/DATA.BIN"))
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
    let path = "ux0/user/00/savedata/PCSE12345/SLOT1/DATA.BIN";
    fixture.write(path, b"original").await;
    let id = fixture
        .saves
        .infer_save_depot(&Package::new(Some("PCSE12345".into()), None));
    for invalid in [
        "ux0/user/00/savedata/PCSB12345/SLOT2/DATA.BIN",
        "ux0/app/PCSE12345/eboot.bin",
        "ux0/user/01/savedata/PCSE12345/DATA.BIN",
        "ux0/user/00/savedata/PCSE12345/SLOT1/../escape",
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
