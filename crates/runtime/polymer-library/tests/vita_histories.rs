use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use polymer_consolestorage::impls::vita::VitaNand;
use polymer_consolestorage::installable::InstallEvent;
use polymer_consolestorage::installable::artifacts::Artifact;
use polymer_consolestorage::nand::VirtualNand;
use polymer_consolestorage::{
    Error, GitVolumeStorage, PlatformRepository, Storage, StorageKind, VolumeStorage,
};
use polymer_library::keys::{KeyStore, PlatformKeys};
use uuid::Uuid;

struct VitaFixture {
    directory: tempfile::TempDir,
    platform: PlatformRepository,
    volume: GitVolumeStorage,
    nand: VitaNand,
    keys: PlatformKeys,
}

impl VitaFixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let platform =
            PlatformRepository::open(directory.path().join("platform"), VitaNand::PLATFORM)
                .unwrap();
        let id = Uuid::new_v4();
        let volume = platform.volume(StorageKind::Nand, id);
        let nand = VitaNand::new_empty(volume.clone(), id, None).await.unwrap();
        let keys = KeyStore::open(directory.path().join("keys"))
            .await
            .unwrap()
            .for_platform(VitaNand::PLATFORM)
            .unwrap();
        Self {
            directory,
            platform,
            volume,
            nand,
            keys,
        }
    }

    fn package(
        &self,
        title: &str,
        category: &str,
        version: &str,
        suffix: &str,
        files: &[(&str, &[u8])],
    ) -> PathBuf {
        let path = self.directory.path().join(Uuid::new_v4().to_string());
        fs::create_dir_all(path.join("sce_sys")).unwrap();
        let content = format!("EP0000-{title}_00-{suffix}");
        let values = [
            ("TITLE_ID", title),
            ("CONTENT_ID", &content),
            ("CATEGORY", category),
            ("APP_VER", version),
            ("TITLE", title),
        ];
        let mut keys = Vec::new();
        let mut data = Vec::new();
        let mut entries = Vec::new();
        for (key, value) in values {
            entries.extend_from_slice(&(keys.len() as u16).to_le_bytes());
            entries.extend_from_slice(&0x0204u16.to_le_bytes());
            let length = value.len() + 1;
            let capacity = length.next_multiple_of(4);
            entries.extend_from_slice(&(length as u32).to_le_bytes());
            entries.extend_from_slice(&(capacity as u32).to_le_bytes());
            entries.extend_from_slice(&(data.len() as u32).to_le_bytes());
            keys.extend_from_slice(key.as_bytes());
            keys.push(0);
            data.extend_from_slice(value.as_bytes());
            data.resize(data.len() + capacity - value.len(), 0);
        }
        let key_offset = 20 + entries.len();
        let data_offset = (key_offset + keys.len()).next_multiple_of(4);
        let mut bytes = Vec::new();
        for value in [
            0x46535000u32,
            0x101,
            key_offset as u32,
            data_offset as u32,
            values.len() as u32,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend(entries);
        bytes.extend(keys);
        bytes.resize(data_offset, 0);
        bytes.extend(data);
        fs::write(path.join("sce_sys/param.sfo"), bytes).unwrap();
        for (name, bytes) in files {
            let file = path.join(name);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, bytes).unwrap();
        }
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
async fn patch_revisions_restore_without_changing_base_or_dlc() {
    let fixture = VitaFixture::new().await;
    let base = fixture.package(
        "PCSE00001",
        "gd",
        "01.00",
        "BASE000000000000",
        &[("eboot.bin", b"base")],
    );
    let base = fixture.install(&base).await.unwrap();
    let first = fixture.package(
        "PCSE00001",
        "gp",
        "01.01",
        "BASE000000000000",
        &[("eboot.bin", b"patch one"), ("obsolete.bin", b"obsolete")],
    );
    let patch = fixture.install(&first).await.unwrap();
    let addon = fixture.package(
        "PCSE00001",
        "ac",
        "01.00",
        "DLC0000000000001",
        &[("data.bin", b"dlc")],
    );
    let addon = fixture.install(&addon).await.unwrap();
    let unrelated = fixture.package(
        "PCSE00002",
        "gd",
        "01.00",
        "BASE000000000000",
        &[("eboot.bin", b"other")],
    );
    let unrelated = fixture.install(&unrelated).await.unwrap();
    let second = fixture.package(
        "PCSE00001",
        "gp",
        "01.02",
        "BASE000000000000",
        &[("eboot.bin", b"patch two")],
    );
    fixture.install(&second).await.unwrap();
    assert!(!fixture.volume.exists("ux0/patch/PCSE00001/obsolete.bin"));
    let registry = fixture.volume.artifacts();
    let history = registry.history(patch.id()).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].artifact.version.as_deref(), Some("01.02"));
    assert_eq!(registry.history(addon.id()).await.unwrap().len(), 1);
    assert_eq!(registry.history(base.id()).await.unwrap().len(), 1);
    registry
        .restore(patch.id(), &history[1].revision)
        .await
        .unwrap();
    assert_eq!(fixture.read("ux0/patch/PCSE00001/eboot.bin"), b"patch one");
    assert_eq!(
        fixture.read("ux0/patch/PCSE00001/obsolete.bin"),
        b"obsolete"
    );
    assert_eq!(fixture.read("ux0/app/PCSE00001/eboot.bin"), b"base");
    assert_eq!(
        fixture.read("ux0/addcont/PCSE00001/DLC0000000000001/data.bin"),
        b"dlc"
    );
    assert!(
        registry
            .restore(addon.id(), &history[1].revision)
            .await
            .is_err()
    );
    assert!(registry.contains(unrelated.id()).await.unwrap());
    registry.uninstall(&[patch.id()]).await.unwrap();
    assert!(!fixture.volume.exists("ux0/patch/PCSE00001"));
    assert!(registry.contains(addon.id()).await.unwrap());
    assert!(!fixture.volume.is_dirty().unwrap());
}

#[tokio::test]
async fn base_removal_reports_only_its_own_components() {
    let fixture = VitaFixture::new().await;
    let mut installed = Vec::new();
    for (title, category, suffix) in [
        ("PCSE00001", "gd", "BASE000000000000"),
        ("PCSE00001", "gp", "BASE000000000000"),
        ("PCSE00001", "ac", "DLC0000000000001"),
        ("PCSE00001", "ac", "DLC0000000000002"),
        ("PCSE00002", "gd", "BASE000000000000"),
    ] {
        let package = fixture.package(
            title,
            category,
            "01.00",
            suffix,
            &[("eboot.bin", b"payload")],
        );
        installed.push(fixture.install(&package).await.unwrap());
    }
    let registry = fixture.volume.artifacts();
    let mut affected = registry.affected(&[installed[0].id()]).await.unwrap();
    affected.sort();
    let mut expected = installed[1..4]
        .iter()
        .map(|artifact| artifact.id().to_owned())
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(affected, expected);
    assert!(matches!(
        registry.uninstall(&[installed[0].id()]).await,
        Err(Error::AffectedDepots { .. })
    ));
    assert!(registry.contains(installed[0].id()).await.unwrap());
    registry
        .uninstall(&installed[..4].iter().map(Artifact::id).collect::<Vec<_>>())
        .await
        .unwrap();
    assert!(!fixture.volume.exists("ux0/app/PCSE00001"));
    assert!(!fixture.volume.exists("ux0/patch/PCSE00001"));
    assert!(!fixture.volume.exists("ux0/addcont/PCSE00001"));
    assert_eq!(fixture.read("ux0/app/PCSE00002/eboot.bin"), b"payload");
    assert_eq!(registry.summaries().await.unwrap().len(), 1);
    assert!(!fixture.volume.is_dirty().unwrap());
}

#[tokio::test]
async fn failed_and_cancelled_extraction_preserve_installed_revision() {
    let fixture = VitaFixture::new().await;
    let base = fixture.package(
        "PCSE00001",
        "gd",
        "01.00",
        "BASE000000000000",
        &[("eboot.bin", b"original")],
    );
    let original = fixture.install(&base).await.unwrap();
    let revision = fixture.volume.artifacts().revision().unwrap();
    for cancel in [true, false] {
        let replacement = fixture.package(
            "PCSE00001",
            "gd",
            "02.00",
            "BASE000000000000",
            &[
                ("eboot.bin", b"new"),
                ("one.bin", b"one"),
                ("two.bin", b"two"),
            ],
        );
        let mut install = fixture
            .nand
            .try_install(&replacement, &fixture.keys)
            .unwrap();
        while let Some(event) = install.next().await {
            if matches!(event.unwrap(), InstallEvent::File(_)) {
                break;
            }
        }
        if cancel {
            drop(install);
        } else {
            fs::remove_dir_all(&replacement).unwrap();
            assert!(install.finish().await.is_err());
        }
        assert_eq!(fixture.read("ux0/app/PCSE00001/eboot.bin"), b"original");
        assert_eq!(fixture.volume.artifacts().revision().unwrap(), revision);
        assert_eq!(
            fixture
                .volume
                .artifacts()
                .history(original.id())
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(!fixture.volume.is_dirty().unwrap());
    }
    assert!(fixture.install(&base).await.is_err());
    assert_eq!(fixture.volume.artifacts().revision().unwrap(), revision);
}

#[tokio::test]
async fn interrupted_publication_and_checkout_recover_after_reopening() {
    for publication in [true, false] {
        let fixture = VitaFixture::new().await;
        let package = fixture.package(
            "PCSE00001",
            "gd",
            "01.00",
            "BASE000000000000",
            &[("eboot.bin", b"original")],
        );
        fixture.install(&package).await.unwrap();
        let replacement = fixture.package(
            "PCSE00001",
            "gd",
            "02.00",
            "BASE000000000000",
            &[("eboot.bin", b"replacement")],
        );
        let lock = if publication {
            fixture.platform.path().join(format!(
                ".git/refs/consoles/nands/v2/{}.lock",
                fixture.volume.id().simple()
            ))
        } else {
            fixture.volume.index_path().with_extension("lock")
        };
        fs::write(&lock, b"held").unwrap();
        assert!(fixture.install(&replacement).await.is_err());
        assert!(fixture.volume.set_label(Some("must wait")).await.is_err());
        assert!(fixture.install(&package).await.is_err());
        let id = fixture.volume.id();
        drop(fixture.nand);
        drop(fixture.volume);
        fs::remove_file(lock).unwrap();
        let platform =
            PlatformRepository::open(fixture.platform.path(), VitaNand::PLATFORM).unwrap();
        let volume = platform.volume(StorageKind::Nand, id);
        let nand = VitaNand::open(volume.clone()).await.unwrap();
        let artifacts = nand.artifacts().await.unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].version.as_deref(), Some("02.00"));
        let mut bytes = Vec::new();
        volume
            .open_read("ux0/app/PCSE00001/eboot.bin")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"replacement");
        assert_eq!(
            volume
                .artifacts()
                .history(&artifacts[0].id)
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(!volume.is_dirty().unwrap());
        volume.recover().unwrap();
        assert!(!volume.is_dirty().unwrap());

        let lock = volume.index_path().with_extension("lock");
        fs::write(&lock, b"held").unwrap();
        assert!(
            volume
                .artifacts()
                .uninstall(&[&artifacts[0].id])
                .await
                .is_err()
        );
        assert!(
            volume
                .artifacts()
                .uninstall(&[&artifacts[0].id])
                .await
                .is_err()
        );
        fs::remove_file(lock).unwrap();
        let reopened = platform.volume(StorageKind::Nand, id);
        let nand = VitaNand::open(reopened.clone()).await.unwrap();
        assert!(nand.artifacts().await.unwrap().is_empty());
        assert!(!reopened.exists("ux0/app/PCSE00001"));
        assert!(!reopened.is_dirty().unwrap());
    }
}

#[tokio::test]
async fn standalone_addons_remain_independent_and_dirty_nand_is_preserved() {
    let fixture = VitaFixture::new().await;
    let one = fixture.package(
        "PCSE00001",
        "ac",
        "01.00",
        "DLC0000000000001",
        &[("data.bin", b"one")],
    );
    let one = fixture.install(&one).await.unwrap();
    let two = fixture.package(
        "PCSE00001",
        "ac",
        "01.00",
        "DLC0000000000002",
        &[("data.bin", b"two")],
    );
    let two = fixture.install(&two).await.unwrap();
    let replacement = fixture.package(
        "PCSE00001",
        "ac",
        "02.00",
        "DLC0000000000001",
        &[("data.bin", b"new")],
    );
    fixture
        .volume
        .install_bytes(b"guest data", "ux0/data/user.bin")
        .unwrap();
    assert!(fixture.install(&replacement).await.is_err());
    assert!(
        fixture
            .volume
            .artifacts()
            .uninstall(&[one.id()])
            .await
            .is_err()
    );
    assert_eq!(fixture.read("ux0/data/user.bin"), b"guest data");
    assert_eq!(
        fixture.read("ux0/addcont/PCSE00001/DLC0000000000001/data.bin"),
        b"one"
    );
    fixture.volume.recover().unwrap();
    fixture.install(&replacement).await.unwrap();
    let history = fixture.volume.artifacts().history(one.id()).await.unwrap();
    fixture
        .volume
        .artifacts()
        .restore(one.id(), &history[1].revision)
        .await
        .unwrap();
    fixture
        .volume
        .artifacts()
        .uninstall(&[one.id()])
        .await
        .unwrap();
    assert_eq!(
        fixture.read("ux0/addcont/PCSE00001/DLC0000000000002/data.bin"),
        b"two"
    );
    assert!(fixture.volume.artifacts().contains(two.id()).await.unwrap());
    assert!(!fixture.volume.is_dirty().unwrap());
}

#[tokio::test]
async fn deleting_nand_removes_its_histories_without_affecting_other_nands() {
    let fixture = VitaFixture::new().await;
    let package = fixture.package(
        "PCSE00001",
        "gd",
        "01.00",
        "BASE000000000000",
        &[("eboot.bin", b"original")],
    );
    fixture.install(&package).await.unwrap();
    let other_id = Uuid::new_v4();
    let other_volume = fixture.platform.volume(StorageKind::Nand, other_id);
    let other = VitaNand::new_empty(other_volume.clone(), other_id, None)
        .await
        .unwrap();
    let artifact = other
        .try_install(&package, &fixture.keys)
        .unwrap()
        .finish()
        .await
        .unwrap();
    fixture
        .platform
        .delete(StorageKind::Nand, fixture.volume.id())
        .unwrap();
    assert!(
        other_volume
            .artifacts()
            .contains(artifact.id())
            .await
            .unwrap()
    );
    assert_eq!(
        other_volume
            .artifacts()
            .history(artifact.id())
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(!other_volume.is_dirty().unwrap());
    let volume = fixture
        .platform
        .volume(StorageKind::Nand, fixture.volume.id());
    let nand = VitaNand::new_empty(volume.clone(), volume.id(), None)
        .await
        .unwrap();
    assert!(nand.artifacts().await.unwrap().is_empty());
    nand.try_install(&package, &fixture.keys)
        .unwrap()
        .finish()
        .await
        .unwrap();
    assert_eq!(
        volume
            .artifacts()
            .history(artifact.id())
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(!volume.is_dirty().unwrap());
}
