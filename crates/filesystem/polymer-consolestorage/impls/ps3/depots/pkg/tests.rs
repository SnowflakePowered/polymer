use aes::Aes128;
use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use futures::StreamExt;
use uuid::Uuid;

use super::*;
use crate::impls::ps3::depots::Ps3RapDepot;
use crate::{PlatformRepository, StorageKind};

const CONTENT_ID: &str = "UP0001-NPUB00001_00-TESTCONTENT00001";
const GAME_ROOT: &str = "dev_hdd0/game/NPUB00001";
const RAP_PATH: &str = "dev_hdd0/home/00000001/exdata/UP0001-NPUB00001_00-TESTCONTENT00001.rap";

struct Fixture {
    directory: tempfile::TempDir,
    platform: PlatformRepository,
    nand: Ps3Nand,
    id: Uuid,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let platform =
            PlatformRepository::open(directory.path().join("SONY_PS3"), "SONY_PS3").unwrap();
        let id = Uuid::now_v7();
        let nand = Ps3Nand::new_empty(platform.volume(StorageKind::Nand, id), id, None)
            .await
            .unwrap();
        Self {
            directory,
            platform,
            nand,
            id,
        }
    }

    fn pkg(
        &self,
        filename: &str,
        content_id: &str,
        kind: u32,
        flags: u32,
        files: &[(&str, &[u8])],
    ) -> PathBuf {
        // Construct a small retail archive independently of pkgrip's parser.
        // Name and payload locations are AES-block aligned; sizes remain exact.
        let mut data = vec![0; files.len() * 32];
        for (index, (name, bytes)) in files.iter().enumerate() {
            let name_offset = data.len();
            data.extend_from_slice(name.as_bytes());
            data.resize(data.len().next_multiple_of(16), 0);
            let file_offset = data.len();
            data.extend_from_slice(bytes);
            data.resize(data.len().next_multiple_of(16), 0);
            let record = &mut data[index * 32..(index + 1) * 32];
            record[..4].copy_from_slice(&(name_offset as u32).to_be_bytes());
            record[4..8].copy_from_slice(&(name.len() as u32).to_be_bytes());
            record[8..16].copy_from_slice(&(file_offset as u64).to_be_bytes());
            record[16..24].copy_from_slice(&(bytes.len() as u64).to_be_bytes());
            record[24..28].copy_from_slice(&1_u32.to_be_bytes());
        }
        let key = [
            0x2e, 0x7b, 0x71, 0xd7, 0xc9, 0xc9, 0xa1, 0x4e, 0xa3, 0x22, 0x1f, 0x18, 0x88, 0x28,
            0xb8, 0xf8,
        ];
        let cipher = Aes128::new(GenericArray::from_slice(&key));
        for (index, block) in data.chunks_mut(16).enumerate() {
            let mut counter = GenericArray::clone_from_slice(&(index as u128).to_be_bytes());
            cipher.encrypt_block(&mut counter);
            for (byte, mask) in block.iter_mut().zip(counter) {
                *byte ^= mask;
            }
        }
        let mut pkg = vec![0; 0x100];
        pkg[..4].copy_from_slice(b"\x7fPKG");
        pkg[4..8].copy_from_slice(&[0x80, 0, 0, 1]);
        pkg[8..12].copy_from_slice(&0xc0_u32.to_be_bytes());
        pkg[12..16].copy_from_slice(&2_u32.to_be_bytes());
        pkg[16..20].copy_from_slice(&24_u32.to_be_bytes());
        pkg[20..24].copy_from_slice(&(files.len() as u32).to_be_bytes());
        pkg[24..32].copy_from_slice(&(0x100 + data.len() as u64 + 32).to_be_bytes());
        pkg[32..40].copy_from_slice(&0x100_u64.to_be_bytes());
        pkg[40..48].copy_from_slice(&(data.len() as u64).to_be_bytes());
        pkg[0x30..0x30 + content_id.len()].copy_from_slice(content_id.as_bytes());
        for (offset, id, value) in [(0xc0, 2_u32, kind), (0xcc, 3, flags)] {
            pkg[offset..offset + 4].copy_from_slice(&id.to_be_bytes());
            pkg[offset + 4..offset + 8].copy_from_slice(&4_u32.to_be_bytes());
            pkg[offset + 8..offset + 12].copy_from_slice(&value.to_be_bytes());
        }
        pkg.extend(data);
        let digest = Sha1::digest(&pkg);
        pkg.extend_from_slice(&digest);
        pkg.extend_from_slice(&[0; 12]);
        let path = self.directory.path().join(filename);
        std::fs::write(&path, pkg).unwrap();
        path
    }

    fn sfo(version: &str) -> Vec<u8> {
        let mut keys = Vec::new();
        let mut values = Vec::new();
        let mut entries = Vec::new();
        for (key, value) in [
            ("TITLE_ID", "NPUB00001"),
            ("TITLE", "Test game"),
            ("APP_VER", version),
        ] {
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
        bytes.extend_from_slice(&3_u32.to_le_bytes());
        bytes.extend(entries);
        bytes.extend(keys);
        bytes.extend(values);
        bytes
    }

    async fn install(&self, depot: &impl Depot) -> Artifact {
        let (outcome, progress) = depot.install(&self.nand);
        let mut progress = Box::into_pin(progress);
        while let Some(path) = progress.next().await {
            path.unwrap();
        }
        Box::into_pin(outcome).await.unwrap()
    }

    fn read(&self, path: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        self.nand
            .root()
            .open_read(path)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }
}

#[tokio::test]
async fn pkg_and_rap_are_independent_persistent_artifacts() {
    let fixture = Fixture::new().await;
    let payload = (0..131_109)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let sfo = Fixture::sfo("01.00");
    let path = fixture.pkg(
        "game.pkg",
        CONTENT_ID,
        5,
        0,
        &[
            ("PARAM.SFO", &sfo),
            ("USRDIR/EBOOT.BIN", &payload),
            ("USRDIR/empty", b""),
            ("ICON0.PNG", b"icon"),
        ],
    );
    let rap = fixture.directory.path().join(format!("{CONTENT_ID}.rap"));
    std::fs::write(&rap, [0x42; 16]).unwrap();
    let license = fixture.install(&Ps3RapDepot::open(&rap).unwrap()).await;
    let depot = Ps3PkgDepot::open(&path).unwrap();
    assert!(depot.validate_contents().is_empty());
    let artifact = fixture.install(&depot).await;
    assert_eq!(artifact.label(), "Test game");
    assert_eq!(artifact.version(), Some("01.00"));
    assert_eq!(artifact.depot_type(), DepotType::App);
    assert_eq!(artifact.manifest().len(), 4);
    for file in artifact.manifest() {
        let bytes = fixture.read(file.path().to_str().unwrap());
        assert_eq!(bytes.len() as u64, file.byte_length());
        assert_eq!(blake3::hash(&bytes).as_bytes(), file.blake3());
    }
    assert_eq!(
        fixture.read(&format!("{GAME_ROOT}/USRDIR/EBOOT.BIN")),
        payload
    );
    assert_eq!(fixture.read(RAP_PATH), [0x42; 16]);
    fixture.nand.root().finish("").unwrap();
    let reopened = Ps3Nand::open(fixture.platform.volume(StorageKind::Nand, fixture.id))
        .await
        .unwrap();
    assert!(
        reopened
            .root()
            .artifacts()
            .artifact(artifact.id())
            .await
            .unwrap()
            .is_some()
    );
    assert!(reopened.root().exists(RAP_PATH));
    // Reinstalling the package never takes ownership of its separate license.
    let repeated = fixture.install(&Ps3PkgDepot::open(&path).unwrap()).await;
    assert!(
        !repeated
            .manifest()
            .iter()
            .any(|file| file.path() == Path::new(RAP_PATH))
    );
    fixture.nand.uninstall(&[artifact.id()]).await.unwrap();
    assert_eq!(fixture.read(RAP_PATH), [0x42; 16]);
    fixture.nand.uninstall(&[license.id()]).await.unwrap();
    assert!(!fixture.nand.root().exists(RAP_PATH));
    assert!(
        !fixture
            .nand
            .root()
            .exists(&format!("{GAME_ROOT}/USRDIR/EBOOT.BIN"))
    );
}

#[tokio::test]
async fn update_and_dlc_have_distinct_artifacts_with_shared_game_directory() {
    let fixture = Fixture::new().await;
    let base = fixture.pkg(
        "base.pkg",
        CONTENT_ID,
        5,
        0,
        &[
            ("USRDIR/EBOOT.BIN", b"base executable"),
            ("USRDIR/base.dat", b"base"),
        ],
    );
    let update = fixture.pkg(
        "update.pkg",
        CONTENT_ID,
        4,
        0x10,
        &[("USRDIR/EBOOT.BIN", b"patch")],
    );
    let dlc = fixture.pkg(
        "dlc.pkg",
        "UP0001-NPUB00001_00-DLC0000000000001",
        4,
        0,
        &[("USRDIR/dlc.dat", b"dlc")],
    );
    let base = fixture.install(&Ps3PkgDepot::open(&base).unwrap()).await;
    let update = fixture.install(&Ps3PkgDepot::open(&update).unwrap()).await;
    let dlc = fixture.install(&Ps3PkgDepot::open(&dlc).unwrap()).await;
    assert_eq!(update.depot_type(), DepotType::AppUpdate);
    assert_eq!(dlc.depot_type(), DepotType::AppContent);
    assert_ne!(base.id(), update.id());
    assert_ne!(update.id(), dlc.id());
    assert_eq!(
        fixture.read(&format!("{GAME_ROOT}/USRDIR/EBOOT.BIN")),
        b"patch"
    );
    assert_eq!(
        fixture.read(&format!("{GAME_ROOT}/USRDIR/base.dat")),
        b"base"
    );
    // The existing NAND dependency policy requires overlapping artifacts to be
    // uninstalled together; an independent DLC remains installed.
    assert!(matches!(
        fixture.nand.uninstall(&[update.id()]).await,
        Err(Error::AffectedDepots { .. })
    ));
    fixture
        .nand
        .uninstall(&[base.id(), update.id()])
        .await
        .unwrap();
    assert_eq!(fixture.read(&format!("{GAME_ROOT}/USRDIR/dlc.dat")), b"dlc");
}

#[tokio::test]
async fn git_discards_failed_and_cancelled_installs_including_licenses() {
    let fixture = Fixture::new().await;
    let initial = fixture.pkg(
        "base.pkg",
        CONTENT_ID,
        5,
        0,
        &[("USRDIR/EBOOT.BIN", b"original")],
    );
    let rap = fixture.directory.path().join(format!("{CONTENT_ID}.rap"));
    std::fs::write(&rap, [1; 16]).unwrap();
    fixture.install(&Ps3RapDepot::open(&rap).unwrap()).await;
    let original = fixture.install(&Ps3PkgDepot::open(&initial).unwrap()).await;
    let update = fixture.pkg(
        "replacement.pkg",
        CONTENT_ID,
        5,
        0,
        &[("USRDIR/EBOOT.BIN", b"new"), ("USRDIR/new.dat", b"new")],
    );
    let depot = Ps3PkgDepot::open(&update).unwrap();
    let (outcome, progress) = depot.install(&fixture.nand);
    let mut progress = Box::into_pin(progress);
    for _ in 0..2 {
        progress.next().await.unwrap().unwrap();
    }
    assert_eq!(fixture.read(RAP_PATH), [1; 16]);
    drop(progress); // Stop after writes but before artifact publication.
    assert!(matches!(
        Box::into_pin(outcome).await,
        Err(Error::InstallIncomplete)
    ));
    // Cancellation leaves only working-tree writes. Git restores the committed
    // NAND when the installation session ends.
    fixture.nand.root().finish("").unwrap();
    assert_eq!(
        fixture.read(&format!("{GAME_ROOT}/USRDIR/EBOOT.BIN")),
        b"original"
    );
    assert_eq!(fixture.read(RAP_PATH), [1; 16]);
    assert!(
        !fixture
            .nand
            .root()
            .exists(&format!("{GAME_ROOT}/USRDIR/new.dat"))
    );
    let failure = fixture.pkg(
        "failure.pkg",
        CONTENT_ID,
        5,
        0,
        &[
            ("USRDIR/EBOOT.BIN", b"new"),
            ("USRDIR", b"file over directory"),
        ],
    );
    // A file/directory collision in the archive is rejected before installation.
    assert!(Ps3PkgDepot::open(&failure).is_err());
    let failure = fixture.pkg(
        "io-failure.pkg",
        CONTENT_ID,
        5,
        0,
        &[
            ("USRDIR/new.dat", b"new"),
            ("USRDIR/EBOOT.BIN/child", b"blocked"),
        ],
    );
    let depot = Ps3PkgDepot::open(&failure).unwrap();
    let (outcome, progress) = depot.install(&fixture.nand);
    let results = Box::into_pin(progress).collect::<Vec<_>>().await;
    assert!(results.last().unwrap().is_err());
    assert!(Box::into_pin(outcome).await.is_err());
    fixture.nand.root().finish("").unwrap();
    assert!(
        !fixture
            .nand
            .root()
            .exists(&format!("{GAME_ROOT}/USRDIR/new.dat"))
    );
    assert_eq!(
        fixture.read(&format!("{GAME_ROOT}/USRDIR/EBOOT.BIN")),
        b"original"
    );
    assert_eq!(fixture.read(RAP_PATH), [1; 16]);
    fixture.nand.uninstall(&[original.id()]).await.unwrap();
}

#[tokio::test]
async fn rejects_corruption_wrong_platform_paths_and_invalid_rap() {
    let fixture = Fixture::new().await;
    let path = fixture.pkg("game.pkg", CONTENT_ID, 5, 0, &[("USRDIR/data", b"payload")]);
    let depot = Ps3PkgDepot::open(&path).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    let last_payload = bytes.len() - 33;
    bytes[last_payload] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(!depot.validate_contents().is_empty());
    let (outcome, progress) = depot.install(&fixture.nand);
    assert!(Box::into_pin(progress).next().await.unwrap().is_err());
    assert!(Box::into_pin(outcome).await.is_err());
    assert!(!fixture.nand.root().exists(GAME_ROOT));
    let path = fixture.pkg("unsafe.pkg", CONTENT_ID, 5, 0, &[("../escape", b"bad")]);
    assert!(Ps3PkgDepot::open(&path).is_err());
    let path = fixture.pkg("theme.pkg", CONTENT_ID, 9, 0, &[("theme.p3t", b"theme")]);
    assert!(matches!(
        Ps3PkgDepot::open(&path),
        Err(Error::Unsupported(_))
    ));
    let path = fixture.pkg("valid.pkg", CONTENT_ID, 5, 0, &[("USRDIR/data", b"data")]);
    let rap = fixture.directory.path().join(format!("{CONTENT_ID}.rap"));
    std::fs::write(&rap, [0; 17]).unwrap();
    assert!(Ps3RapDepot::open(&rap).is_err());
    let invalid_name = fixture.directory.path().join("wrong.rap");
    std::fs::write(&invalid_name, [0; 16]).unwrap();
    assert!(Ps3RapDepot::open(&invalid_name).is_err());
    let wii_id = Uuid::now_v7();
    let wii_platform = PlatformRepository::open(
        fixture.directory.path().join("NINTENDO_WII"),
        "NINTENDO_WII",
    )
    .unwrap();
    let wii = crate::impls::wii::WiiNand::new_empty(
        wii_platform.volume(StorageKind::Nand, wii_id),
        wii_id,
        None,
    )
    .await
    .unwrap();
    let depot = Ps3PkgDepot::open(&path).unwrap();
    let (outcome, progress) = depot.install(&wii);
    assert!(Box::into_pin(progress).next().await.unwrap().is_err());
    assert!(Box::into_pin(outcome).await.is_err());
}
