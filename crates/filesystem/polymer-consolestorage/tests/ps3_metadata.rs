use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::sync::Arc;

use futures::stream;
use polymer_consolestorage::impls::ps3::Ps3Nand;
use polymer_consolestorage::savegame::icons::SaveIcon;
use polymer_consolestorage::savegame::{Package, SaveBlob, SaveDepotId};
use polymer_consolestorage::{PlatformRepository, SaveRepo, StorageKind, VersionedStorage};
use uuid::Uuid;

struct Fixture {
    _directory: tempfile::TempDir,
    saves: Arc<dyn SaveRepo>,
    family: SaveDepotId,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let platform =
            PlatformRepository::open(directory.path().join("SONY_PS3"), "SONY_PS3").unwrap();
        let id = Uuid::now_v7();
        let saves = Ps3Nand::new_save_repo(
            VersionedStorage::new(platform.volume(StorageKind::SaveRepo, id)).unwrap(),
            id,
            None,
        )
        .await
        .unwrap();
        let family = saves.infer_save_depot(&Package::new(Some("BLES00001".to_owned()), None));
        Self {
            _directory: directory,
            saves,
            family,
        }
    }

    fn blob(&self, name: &str, bytes: Vec<u8>) -> SaveBlob {
        SaveBlob {
            target_path: PathBuf::from(format!(
                "dev_hdd0/home/00000001/savedata/BLES00001-slot/{name}"
            )),
            blake3: *blake3::hash(&bytes).as_bytes(),
            bytes,
        }
    }

    fn sfo(fields: &[(&str, &str)]) -> Vec<u8> {
        let mut keys = Vec::new();
        let mut values = Vec::new();
        let mut entries = Vec::new();
        for (key, value) in fields {
            entries.extend_from_slice(&(keys.len() as u16).to_le_bytes());
            entries.extend_from_slice(&0x0204_u16.to_le_bytes());
            entries.extend_from_slice(&(value.len() as u32 + 1).to_le_bytes());
            entries.extend_from_slice(&(value.len() as u32 + 1).to_le_bytes());
            entries.extend_from_slice(&(values.len() as u32).to_le_bytes());
            keys.extend_from_slice(key.as_bytes());
            keys.push(0);
            values.extend_from_slice(value.as_bytes());
            values.push(0);
        }
        let mut bytes = b"\0PSF\x01\x01\0\0".to_vec();
        bytes.extend_from_slice(&(20 + entries.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(20 + entries.len() as u32 + keys.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(fields.len() as u32).to_le_bytes());
        bytes.extend(entries);
        bytes.extend(keys);
        bytes.extend(values);
        bytes
    }
}

#[tokio::test]
async fn metadata_and_icon_are_read_without_losing_saves_with_invalid_sfo() {
    let fixture = Fixture::new().await;
    let sfo = Fixture::sfo(&[("TITLE", "Game title"), ("SUB_TITLE", "Slot description")]);
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        3,
        image::Rgba([1, 2, 3, 255]),
    ))
    .write_to(&mut png, image::ImageFormat::Png)
    .unwrap();
    let descriptor = fixture
        .saves
        .install_save(Box::pin(stream::iter(vec![
            fixture.blob("PARAM.SFO", sfo.clone()),
            fixture.blob("ICON0.PNG", png.into_inner()),
            fixture.blob("DATA.BIN", b"save data".to_vec()),
        ])))
        .await
        .unwrap();
    assert_eq!(descriptor.title, "Game title");
    assert_eq!(descriptor.description, "Slot description");
    let Some(SaveIcon::Image(icon)) = fixture.saves.read_save_icon(fixture.family).unwrap() else {
        panic!("expected a decoded PNG icon");
    };
    assert_eq!(icon.dimensions(), (2, 3));

    let mut corrupt = sfo;
    corrupt[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
    fixture
        .saves
        .install_save(Box::pin(stream::iter(vec![
            fixture.blob("PARAM.SFO", corrupt),
        ])))
        .await
        .unwrap();
    let saves = fixture.saves.enumerate_saves().collect::<Vec<_>>();
    assert_eq!(saves.len(), 1);
    assert_eq!(saves[0].id, fixture.family);
    assert_eq!(saves[0].title, "BLES00001-slot");
    assert_eq!(
        fixture
            .saves
            .export_save(&fixture.family)
            .unwrap()
            .blobs
            .len(),
        3
    );
}

#[tokio::test]
async fn invalid_import_batches_do_not_replace_live_save_files() {
    let fixture = Fixture::new().await;
    let original = fixture.blob("DATA.BIN", b"original".to_vec());
    let original_path = original.target_path.to_str().unwrap().to_owned();
    fixture
        .saves
        .install_save(Box::pin(stream::iter(vec![original])))
        .await
        .unwrap();
    let mut corrupt = fixture.blob("SECOND.BIN", b"corrupt".to_vec());
    corrupt.blake3 = [0; 32];
    let mut misplaced = fixture.blob("SECOND.BIN", b"misplaced".to_vec());
    misplaced.target_path =
        PathBuf::from("dev_hdd0/home/00000001/savedata/BLUS00002-slot/SECOND.BIN");
    let traversal = fixture.blob("../escape", b"escape".to_vec());
    for invalid in [corrupt, misplaced, traversal] {
        let replacement = fixture.blob("DATA.BIN", b"replacement".to_vec());
        assert!(
            fixture
                .saves
                .install_save(Box::pin(stream::iter(vec![replacement, invalid])))
                .await
                .is_err()
        );
        let mut bytes = Vec::new();
        fixture
            .saves
            .open_read(&original_path)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"original");
        assert_eq!(
            fixture
                .saves
                .export_save(&fixture.family)
                .unwrap()
                .blobs
                .len(),
            1
        );
    }
}
