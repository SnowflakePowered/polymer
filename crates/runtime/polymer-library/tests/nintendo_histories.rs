use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use polymer_consolestorage::impls::{
    ctr::{CiaDepot, CtrNand, ticket::CtrTicket},
    wiiu::{depots::WiiUMetaXmlDepot, nand::WiiUNand},
};
use polymer_consolestorage::installable::{Depot, InstallEvent, artifacts::Artifact};
use polymer_consolestorage::nand::VirtualNand;
use polymer_consolestorage::{
    Error, GitVolumeStorage, PlatformRepository, Storage, StorageKind, VolumeStorage,
};
use polymer_library::keys::{KeyStore, PlatformKeys};
use sha2::{Digest, Sha256};
use uuid::Uuid;

struct NintendoFixture {
    directory: tempfile::TempDir,
    volume: GitVolumeStorage,
    nand: Box<dyn VirtualNand>,
    keys: PlatformKeys,
}

impl NintendoFixture {
    async fn new(platform: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let repository =
            PlatformRepository::open(directory.path().join("platform"), platform).unwrap();
        let id = Uuid::new_v4();
        let volume = repository.volume(StorageKind::Nand, id);
        let nand: Box<dyn VirtualNand> = if platform == WiiUNand::PLATFORM {
            Box::new(WiiUNand::new_empty(volume.clone(), id, None).await.unwrap())
        } else {
            Box::new(
                CtrNand::create(volume.clone(), id, platform, None)
                    .await
                    .unwrap(),
            )
        };
        let keys = KeyStore::open(directory.path().join("keys"))
            .await
            .unwrap()
            .for_platform(platform)
            .unwrap();
        Self {
            directory,
            volume,
            nand,
            keys,
        }
    }

    fn wiiu(&self, id: u64, version: u16, files: &[(&str, &[u8])]) -> PathBuf {
        let path = self.directory.path().join(Uuid::new_v4().to_string());
        fs::create_dir_all(path.join("code")).unwrap();
        fs::create_dir_all(path.join("content")).unwrap();
        fs::create_dir_all(path.join("meta")).unwrap();
        let base = 0x0005000000000000u64 | (id & 0xffffffff);
        fs::write(
            path.join("code/app.xml"),
            format!("<app><title_id>{id:016x}</title_id></app>"),
        )
        .unwrap();
        fs::write(path.join("meta/meta.xml"), format!("<menu><title_id>{base:016x}</title_id><title_version>{version}</title_version></menu>")).unwrap();
        fs::write(
            path.join("code/cos.xml"),
            "<app><argstr>main.rpx</argstr></app>",
        )
        .unwrap();
        fs::write(path.join("code/main.rpx"), version.to_le_bytes()).unwrap();
        for (name, bytes) in files {
            fs::write(path.join(name), bytes).unwrap();
        }
        path
    }

    fn cia(&self, id: u64, version: u16, contents: &[(u32, &[u8])], present: &[usize]) -> PathBuf {
        let mut records = Vec::new();
        for (index, (content_id, bytes)) in contents.iter().enumerate() {
            records.extend_from_slice(&content_id.to_be_bytes());
            records.extend_from_slice(&(index as u16).to_be_bytes());
            records.extend_from_slice(&0x4000u16.to_be_bytes());
            records.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
            records.extend_from_slice(&Sha256::digest(bytes));
        }
        let mut infos = vec![0; 0x900];
        infos[2..4].copy_from_slice(&(contents.len() as u16).to_be_bytes());
        infos[4..36].copy_from_slice(&Sha256::digest(&records));
        let mut tmd = vec![0; 0x140 + 0xc4];
        tmd[..4].copy_from_slice(&0x10004u32.to_be_bytes());
        tmd[0x180] = 1;
        tmd[0x18c..0x194].copy_from_slice(&id.to_be_bytes());
        tmd[0x1dc..0x1de].copy_from_slice(&version.to_be_bytes());
        tmd[0x1de..0x1e0].copy_from_slice(&(contents.len() as u16).to_be_bytes());
        tmd[0x1e4..0x204].copy_from_slice(&Sha256::digest(&infos));
        tmd.extend(infos);
        tmd.extend(records);
        let ticket = CtrTicket::synthesize(id, &[0; 16]).unwrap();
        let mut archive = vec![0; 0x2020];
        archive[..4].copy_from_slice(&0x2020u32.to_le_bytes());
        archive[12..16].copy_from_slice(&(ticket.len() as u32).to_le_bytes());
        archive[16..20].copy_from_slice(&(tmd.len() as u32).to_le_bytes());
        let length: usize = present.iter().map(|&index| contents[index].1.len()).sum();
        archive[24..32].copy_from_slice(&(length as u64).to_le_bytes());
        for &index in present {
            archive[32 + index / 8] |= 0x80 >> (index % 8);
        }
        for section in [ticket.as_slice(), tmd.as_slice()] {
            archive.resize(archive.len().next_multiple_of(64), 0);
            archive.extend_from_slice(section);
        }
        archive.resize(archive.len().next_multiple_of(64), 0);
        for &index in present {
            archive.extend_from_slice(contents[index].1);
        }
        let path = self
            .directory
            .path()
            .join(format!("{}.cia", Uuid::new_v4()));
        fs::write(&path, archive).unwrap();
        path
    }

    async fn install(&self, path: &Path) -> Result<Artifact, Error> {
        self.nand.try_install(path, &self.keys)?.finish().await
    }

    fn read(&self, path: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        self.volume
            .open_read(path)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }
}

#[tokio::test]
async fn wiiu_updates_replace_and_restore_independently() {
    let fixture = NintendoFixture::new(WiiUNand::PLATFORM).await;
    let base = fixture.wiiu(0x0005000012345600, 1, &[]);
    let base = fixture.install(&base).await.unwrap();
    let first = fixture.wiiu(0x0005000e12345600, 2, &[("content/old.bin", b"old")]);
    let root = WiiUMetaXmlDepot::open(&first).unwrap().install_path();
    let patch = fixture.install(&first).await.unwrap();
    let dlc = fixture.wiiu(0x0005000c12345600, 1, &[("content/dlc.bin", b"addon")]);
    let dlc = fixture.install(&dlc).await.unwrap();
    let other = fixture.wiiu(0x0005000012345700, 1, &[]);
    let other = fixture.install(&other).await.unwrap();
    let next = fixture.wiiu(0x0005000e12345600, 3, &[("content/new.bin", b"new")]);
    fixture.install(&next).await.unwrap();
    assert!(!fixture.volume.exists(&format!("{root}/content/old.bin")));
    assert!(fixture.install(&next).await.is_err());
    let registry = fixture.volume.artifacts();
    let history = registry.history(patch.id()).await.unwrap();
    assert_eq!(history.len(), 2);
    registry
        .restore(patch.id(), &history[1].revision)
        .await
        .unwrap();
    assert_eq!(fixture.read(&format!("{root}/content/old.bin")), b"old");
    assert!(!fixture.volume.exists(&format!("{root}/content/new.bin")));
    assert_eq!(registry.history(base.id()).await.unwrap().len(), 1);
    assert_eq!(registry.history(dlc.id()).await.unwrap().len(), 1);
    assert_eq!(
        fixture.read("usr/title/0005000c/12345600/content/dlc.bin"),
        b"addon"
    );
    let mut affected = registry.affected(&[base.id()]).await.unwrap();
    affected.sort();
    assert_eq!(affected, vec![dlc.id(), patch.id()]);
    registry.uninstall(&[patch.id()]).await.unwrap();
    assert!(registry.contains(dlc.id()).await.unwrap());
    registry.uninstall(&[base.id(), dlc.id()]).await.unwrap();
    assert!(registry.contains(other.id()).await.unwrap());
    assert!(!fixture.volume.is_dirty().unwrap());
}

#[tokio::test]
async fn ctr_revisions_preserve_other_components_on_both_models() {
    for platform in [CtrNand::PLATFORM, CtrNand::NEW_PLATFORM] {
        let fixture = NintendoFixture::new(platform).await;
        let base = fixture.cia(0x0004000012345600, 1, &[(1, b"base")], &[0]);
        let base_root = CiaDepot::open(&base).unwrap().install_path().to_owned();
        let save_path = format!(
            "{}/data/save.bin",
            base_root.strip_suffix("/content").unwrap()
        );
        let base = fixture.install(&base).await.unwrap();
        fixture
            .volume
            .install_bytes(b"saved game", &save_path)
            .unwrap();
        fixture
            .volume
            .set_label(Some("with existing save"))
            .await
            .unwrap();
        let patch = fixture.cia(
            0x0004000e12345600,
            2,
            &[(2, b"first"), (3, b"obsolete")],
            &[0, 1],
        );
        let root = CiaDepot::open(&patch).unwrap().install_path().to_owned();
        let patch = fixture.install(&patch).await.unwrap();
        let dlc = fixture.cia(0x0004008c12345600, 1, &[(4, b"dlc")], &[0]);
        let dlc = fixture.install(&dlc).await.unwrap();
        let next = fixture.cia(0x0004000e12345600, 3, &[(5, b"second")], &[0]);
        fixture.install(&next).await.unwrap();
        assert!(
            !fixture
                .volume
                .exists(&format!("{root}/00000000/00000003.app"))
        );
        let registry = fixture.volume.artifacts();
        let history = registry.history(patch.id()).await.unwrap();
        assert_eq!(history.len(), 2);
        registry
            .restore(patch.id(), &history[1].revision)
            .await
            .unwrap();
        assert_eq!(
            fixture.read(&format!("{root}/00000000/00000002.app")),
            b"first"
        );
        assert!(!fixture.volume.exists(&format!("{root}/00000005.app")));
        assert_eq!(registry.history(base.id()).await.unwrap().len(), 1);
        assert_eq!(registry.history(dlc.id()).await.unwrap().len(), 1);
        assert_eq!(registry.affected(&[base.id()]).await.unwrap().len(), 2);
        registry.uninstall(&[patch.id()]).await.unwrap();
        assert!(registry.contains(dlc.id()).await.unwrap());
        registry.uninstall(&[base.id(), dlc.id()]).await.unwrap();
        assert!(registry.summaries().await.unwrap().is_empty());
        assert_eq!(fixture.read(&save_path), b"saved game");
        assert!(!fixture.volume.is_dirty().unwrap());
    }
}

#[tokio::test]
async fn nintendo_cancelled_and_failed_extraction_leave_the_selected_revision_intact() {
    for platform in [WiiUNand::PLATFORM, CtrNand::PLATFORM] {
        let fixture = NintendoFixture::new(platform).await;
        let initial = if platform == WiiUNand::PLATFORM {
            fixture.wiiu(0x0005000012345600, 1, &[])
        } else {
            fixture.cia(0x0004000012345600, 1, &[(1, b"initial")], &[0])
        };
        let artifact = fixture.install(&initial).await.unwrap();
        let revision = fixture.volume.artifacts().revision().unwrap();
        for cancel in [true, false] {
            let replacement = if platform == WiiUNand::PLATFORM {
                fixture.wiiu(0x0005000012345600, 2, &[("content/extra.bin", b"extra")])
            } else {
                fixture.cia(
                    0x0004000012345600,
                    2,
                    &[(2, b"new"), (3, b"extra")],
                    &[0, 1],
                )
            };
            let mut install = fixture
                .nand
                .try_install(&replacement, &fixture.keys)
                .unwrap();
            let mut extracted = false;
            while let Some(event) = install.next().await {
                if matches!(event.unwrap(), InstallEvent::File(_)) {
                    extracted = true;
                    break;
                }
            }
            assert!(extracted);
            if cancel {
                drop(install);
            } else {
                if replacement.is_dir() {
                    fs::remove_dir_all(&replacement).unwrap();
                } else {
                    fs::OpenOptions::new()
                        .write(true)
                        .open(&replacement)
                        .unwrap()
                        .set_len(0)
                        .unwrap();
                }
                assert!(install.finish().await.is_err());
            }
            assert_eq!(fixture.volume.artifacts().revision().unwrap(), revision);
            assert_eq!(
                fixture
                    .volume
                    .artifacts()
                    .history(artifact.id())
                    .await
                    .unwrap()
                    .len(),
                1
            );
            for file in artifact.manifest() {
                assert_eq!(
                    blake3::hash(&fixture.read(&file.path().to_string_lossy())).as_bytes(),
                    file.blake3()
                );
            }
            assert!(!fixture.volume.is_dirty().unwrap());
        }
    }
}

#[tokio::test]
async fn partial_ctr_dlc_extends_same_version_and_rolls_back() {
    let fixture = NintendoFixture::new(CtrNand::PLATFORM).await;
    let contents: &[(u32, &[u8])] = &[(10, b"one"), (11, b"two"), (12, b"three")];
    let first = fixture.cia(0x0004008c12345600, 1, contents, &[0]);
    let root = CiaDepot::open(&first).unwrap().install_path().to_owned();
    let artifact = fixture.install(&first).await.unwrap();
    let second = fixture.cia(0x0004008c12345600, 1, contents, &[1]);
    fixture.install(&second).await.unwrap();
    assert_eq!(
        fixture.read(&format!("{root}/00000000/0000000a.app")),
        b"one"
    );
    assert_eq!(
        fixture.read(&format!("{root}/00000000/0000000b.app")),
        b"two"
    );
    assert!(fixture.install(&second).await.is_err());
    let registry = fixture.volume.artifacts();
    let history = registry.history(artifact.id()).await.unwrap();
    assert_eq!(history.len(), 2);
    registry
        .restore(artifact.id(), &history[1].revision)
        .await
        .unwrap();
    assert!(
        !fixture
            .volume
            .exists(&format!("{root}/00000000/0000000b.app"))
    );
    let changed = fixture.cia(
        0x0004008c12345600,
        2,
        &[(10, b"changed"), (11, b"two")],
        &[1],
    );
    fixture.install(&changed).await.unwrap();
    assert!(
        !fixture
            .volume
            .exists(&format!("{root}/00000000/0000000a.app"))
    );
    assert_eq!(
        fixture.read(&format!("{root}/00000000/0000000b.app")),
        b"two"
    );
}
