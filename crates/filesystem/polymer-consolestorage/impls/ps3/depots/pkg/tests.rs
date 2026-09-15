use super::*;
use crate::impls::sony::metadata::SonyMetadata;
use crate::nand::VirtualNand;
use crate::{PlatformRepository, StorageKind};
use aes::cipher::{KeyIvInit, StreamCipher};
use uuid::Uuid;

struct Fixture;

impl Fixture {
    fn package(path: &Path, files: &[(&str, Vec<u8>)]) -> Ps3PkgDepot {
        let mut data = vec![0; files.len() * 32];
        for (index, (name, contents)) in files.iter().enumerate() {
            let name_offset = data.len();
            data.extend(name.as_bytes());
            data.resize((data.len() + 15) & !15, 0);
            let file_offset = data.len();
            data.extend(contents);
            data.resize((data.len() + 15) & !15, 0);
            let entry = &mut data[index * 32..(index + 1) * 32];
            entry[..4].copy_from_slice(&(name_offset as u32).to_be_bytes());
            entry[4..8].copy_from_slice(&(name.len() as u32).to_be_bytes());
            entry[12..16].copy_from_slice(&(file_offset as u32).to_be_bytes());
            entry[20..24].copy_from_slice(&(contents.len() as u32).to_be_bytes());
            entry[24..28].copy_from_slice(&1_u32.to_be_bytes());
        }
        let key = [
            0x2e, 0x7b, 0x71, 0xd7, 0xc9, 0xc9, 0xa1, 0x4e, 0xa3, 0x22, 0x1f, 0x18, 0x88, 0x28,
            0xb8, 0xf8,
        ];
        ctr::Ctr128BE::<aes::Aes128>::new(&key.into(), &[0; 16].into()).apply_keystream(&mut data);
        let mut bytes = vec![0; 0x100];
        bytes[..8].copy_from_slice(&[0x7f, b'P', b'K', b'G', 0x80, 0, 0, 1]);
        bytes[8..12].copy_from_slice(&0x80_u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&2_u32.to_be_bytes());
        bytes[16..20].copy_from_slice(&24_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&(files.len() as u32).to_be_bytes());
        bytes[0x24..0x28].copy_from_slice(&0x100_u32.to_be_bytes());
        bytes[0x30..0x54].copy_from_slice(b"UP0001-NPUB12345_00-0123456789ABCDEF");
        for (index, (kind, value)) in [(2_u32, 5_u32), (3, 0x10)].into_iter().enumerate() {
            let start = 0x80 + index * 12;
            bytes[start..start + 4].copy_from_slice(&kind.to_be_bytes());
            bytes[start + 4..start + 8].copy_from_slice(&4_u32.to_be_bytes());
            bytes[start + 8..start + 12].copy_from_slice(&value.to_be_bytes());
        }
        bytes.extend(data);
        let hash = Sha1::digest(&bytes);
        bytes.extend(hash);
        bytes.extend([0; 12]);
        std::fs::write(path, bytes).unwrap();
        Ps3PkgDepot::open(path).unwrap()
    }
}

#[tokio::test]
async fn patches_retain_earlier_files_and_read_installed_sfo() {
    let directory = tempfile::tempdir().unwrap();
    let platform =
        PlatformRepository::open(directory.path().join("nand.git"), Ps3Nand::PLATFORM).unwrap();
    let id = Uuid::now_v7();
    let nand = Ps3Nand::new_empty(platform.volume(StorageKind::Nand, id), id, None)
        .await
        .unwrap();
    let path = directory.path().join("patch.pkg");
    let first = Fixture::package(&path, &[("old.bin", b"keep".to_vec())]);
    assert!(first.satisfies(&nand).is_err());
    nand.root()
        .create("dev_flash/sys/external/liblv2.sprx")
        .unwrap()
        .write_all_at(0, b"system")
        .unwrap();
    nand.set_label(Some("Test firmware")).await.unwrap();
    let artifact = first.install(&nand).finish().await.unwrap();
    assert_eq!(artifact.label(), "NPUB12345");
    let patch = Fixture::package(
        &path,
        &[
            (
                "PARAM.SFO",
                SonyMetadata::fixture(&[
                    ("TITLE_ID", "NPUB12345"),
                    ("TITLE", "Installed patch"),
                    ("APP_VER", "02.00"),
                ]),
            ),
            ("new.bin", b"new".to_vec()),
        ],
    );
    let artifact = patch.install(&nand).finish().await.unwrap();
    assert_eq!(artifact.label(), "Installed patch");
    assert_eq!(artifact.version(), Some("02.00"));
    assert_eq!(artifact.manifest().len(), 3);
    assert!(
        artifact
            .manifest()
            .iter()
            .any(|file| file.path().ends_with("old.bin"))
    );
    nand.uninstall(&[artifact.id()]).await.unwrap();
    assert!(!nand.root().exists("dev_hdd0/game/NPUB12345/old.bin"));
    assert!(!nand.root().exists("dev_hdd0/game/NPUB12345/new.bin"));
    assert!(nand.root().exists("dev_flash/sys/external/liblv2.sprx"));
}
