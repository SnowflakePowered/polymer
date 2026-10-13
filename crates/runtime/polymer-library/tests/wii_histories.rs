mod wii_support;

use polymer_consolestorage::impls::wii::WiiNand;
use polymer_consolestorage::installable::{DepotType, InstallEvent};
use polymer_consolestorage::nand::{NandCompletion, VirtualNand};
use polymer_consolestorage::{Error, PlatformRepository, Storage, StorageKind, VolumeStorage};
use wii_support::{Wad, WiiFixture};

#[tokio::test]
async fn composed_rename_refuses_pending_save_restore_until_recovery() {
    use polymer_consolestorage::savegame::SaveDepotId;
    use polymer_consolestorage::{Save, VersionedStorage};
    use std::sync::Arc;

    let fixture = WiiFixture::new().await;
    let id = uuid::Uuid::new_v4();
    let profile = fixture.platform.volume(StorageKind::SaveRepo, id);
    let saves = WiiNand::new_save_repo(VersionedStorage::new(profile).unwrap(), id, None)
        .await
        .unwrap();
    let path = "title/00010001/41424344/data/save.bin";
    saves
        .create(path)
        .unwrap()
        .write_all_at(0, b"original")
        .unwrap();
    let save = Save::new(saves.as_ref(), SaveDepotId(0x0001000141424344));
    let first = save.checkpoint().await.unwrap().unwrap();
    saves
        .create(path)
        .unwrap()
        .write_all_at(0, b"modified")
        .unwrap();
    save.checkpoint().await.unwrap().unwrap();
    let mounted = fixture
        .nand
        .compose(Some(saves.clone()), Arc::new(fixture.keys.clone()))
        .await
        .unwrap();
    mounted
        .create("tmp/incoming")
        .unwrap()
        .write_all_at(0, b"incoming")
        .unwrap();
    let lock = fixture.platform.path().join(format!(
        ".git/refs/consoles/profiles/v2/{}.lock",
        id.simple()
    ));
    std::fs::write(&lock, b"held").unwrap();
    assert!(save.restore(first.id).await.is_err());
    assert!(matches!(
        mounted.rename("tmp/incoming", path),
        Err(Error::HistoryConflict(_))
    ));
    assert!(mounted.exists("tmp/incoming"));
    std::fs::remove_file(&lock).unwrap();
    saves.checkpoint_saves().await.unwrap();
    mounted.rename("tmp/incoming", path).unwrap();
    assert!(!mounted.exists("tmp/incoming"));
    let reopened = fixture.platform.volume(StorageKind::SaveRepo, id);
    let mut bytes = [0; 8];
    assert_eq!(
        reopened
            .open(path, false)
            .unwrap()
            .read_at(0, &mut bytes)
            .unwrap(),
        8
    );
    assert_eq!(&bytes, b"incoming");
}

#[tokio::test]
async fn composed_rename_persists_profile_changes_before_reopen() {
    use polymer_consolestorage::VersionedStorage;
    use std::io::Read;
    use std::sync::Arc;

    let fixture = WiiFixture::new().await;
    let id = uuid::Uuid::new_v4();
    let path = "title/00010001/41424344/data/save.bin";
    {
        let profile = fixture.platform.volume(StorageKind::SaveRepo, id);
        let saves = WiiNand::new_save_repo(VersionedStorage::new(profile).unwrap(), id, None)
            .await
            .unwrap();
        saves
            .create(path)
            .unwrap()
            .write_all_at(0, b"old destination")
            .unwrap();
        saves.set_label(Some("baseline")).await.unwrap();
        let mounted = fixture
            .nand
            .compose(Some(saves.clone()), Arc::new(fixture.keys.clone()))
            .await
            .unwrap();
        saves
            .create("title/00010001/41424344/data/other.bin")
            .unwrap()
            .write_all_at(0, b"pending edit")
            .unwrap();
        let source = mounted.create("tmp/save.tmp").unwrap();
        source.write_all_at(0, b"replacement").unwrap();
        mounted.rename("tmp/save.tmp", path).unwrap();
        assert!(!mounted.exists("tmp/save.tmp"));
        source.write_all_at(0, b"updated").unwrap();
        source.set_len(7).unwrap();
        mounted.sync(path).unwrap();
    }
    {
        let profile = fixture.platform.volume(StorageKind::SaveRepo, id);
        let mut contents = String::new();
        profile
            .open_read(path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "updated");
        contents.clear();
        profile
            .open_read("title/00010001/41424344/data/other.bin")
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "pending edit");
        let saves = WiiNand::open_save_repo(VersionedStorage::new(profile).unwrap())
            .await
            .unwrap();
        let mounted = fixture
            .nand
            .compose(Some(saves), Arc::new(fixture.keys.clone()))
            .await
            .unwrap();
        mounted.rename(path, "tmp/removed-save").unwrap();
    }
    let reopened = fixture.platform.volume(StorageKind::SaveRepo, id);
    assert!(!reopened.exists(path));
    assert!(reopened.exists("title/00010001/41424344/data/other.bin"));
    assert!(
        reopened
            .read_dir("", true)
            .unwrap()
            .all(|entry| !entry.name.contains(".polymer-"))
    );
}

#[tokio::test]
async fn shared_content_survives_other_title_replacement_and_removal() {
    let fixture = WiiFixture::new().await;
    let a = fixture
        .install(
            Wad::new(0x0001000141424344, 1)
                .content(1, true, b"common")
                .content(2, false, b"obsolete"),
        )
        .await
        .unwrap();
    let common = fixture.shared_path(b"common").unwrap();
    let b = fixture
        .install(Wad::new(0x000100015758595a, 1).content(9, true, b"common"))
        .await
        .unwrap();
    assert_eq!(fixture.shared_path(b"common").unwrap(), common);
    assert!(
        b.manifest()
            .iter()
            .any(|file| file.path().to_string_lossy().replace('\\', "/") == common)
    );
    let save = "title/00010001/41424344/data/save.bin";
    fixture.volume.install_bytes(b"saved game", save).unwrap();
    fixture.volume.set_label(Some("with save")).await.unwrap();
    let uid = fixture.read("sys/uid.sys");
    fixture
        .install(Wad::new(0x0001000141424344, 2).content(3, true, b"replacement"))
        .await
        .unwrap();
    let replacement = fixture.shared_path(b"replacement").unwrap();
    assert_eq!(fixture.read(&common), b"common");
    assert!(
        !fixture
            .volume
            .exists("title/00010001/41424344/content/00000002.app")
    );
    let registry = fixture.volume.artifacts();
    let history = registry.history(a.id()).await.unwrap();
    registry
        .restore(a.id(), &history[1].revision)
        .await
        .unwrap();
    assert_eq!(
        fixture.read("title/00010001/41424344/content/00000002.app"),
        b"obsolete"
    );
    assert!(!fixture.volume.exists(&replacement));
    registry.uninstall(&[a.id()]).await.unwrap();
    assert_eq!(fixture.read(&common), b"common");
    assert_eq!(fixture.read(save), b"saved game");
    assert_eq!(fixture.read("sys/uid.sys"), uid);
    registry.uninstall(&[b.id()]).await.unwrap();
    assert!(!fixture.volume.exists(&common));
    assert!(fixture.read("shared1/content.map").is_empty());
    fixture
        .install(Wad::new(0x0001000141424344, 3).content(1, true, b"common"))
        .await
        .unwrap();
    assert_eq!(fixture.shared_path(b"common").unwrap(), common);
    assert_eq!(fixture.read("sys/uid.sys"), uid);
    assert!(fixture.nand.is_complete(NandCompletion::Minimal));
    assert!(!fixture.volume.is_dirty().unwrap());
}

#[tokio::test]
async fn ios_removal_reports_users_but_titles_and_dlc_have_separate_histories() {
    let fixture = WiiFixture::new().await;
    let ios = fixture
        .install(Wad::new(0x0000000100000038, 1))
        .await
        .unwrap();
    let mut channel = Wad::new(0x0001000141424344, 1);
    channel.ios = 0x0000000100000038;
    let channel = fixture.install(channel).await.unwrap();
    let dlc = fixture
        .install(Wad::new(0x000100057758595a, 1))
        .await
        .unwrap();
    assert_eq!(dlc.depot_type(), DepotType::AppContent);
    assert!(dlc.entrypoint().is_none());
    let registry = fixture.volume.artifacts();
    assert_eq!(
        registry.affected(&[ios.id()]).await.unwrap(),
        vec![channel.id()]
    );
    assert!(matches!(
        registry.uninstall(&[ios.id()]).await,
        Err(Error::AffectedDepots { .. })
    ));
    fixture
        .install(Wad::new(0x0000000100000038, 2))
        .await
        .unwrap();
    let history = registry.history(ios.id()).await.unwrap();
    registry
        .restore(ios.id(), &history[1].revision)
        .await
        .unwrap();
    assert!(registry.contains(channel.id()).await.unwrap());
    registry.uninstall(&[ios.id(), channel.id()]).await.unwrap();
    assert!(registry.contains(dlc.id()).await.unwrap());
}

#[tokio::test]
async fn failed_and_cancelled_wads_leave_shared_indexes_unchanged() {
    let fixture = WiiFixture::new().await;
    fixture
        .install(Wad::new(0x0001000141424344, 1).content(1, true, b"original"))
        .await
        .unwrap();
    let revision = fixture.volume.artifacts().revision().unwrap();
    let uid = fixture.read("sys/uid.sys");
    let map = fixture.read("shared1/content.map");
    let path = fixture.package(Wad::new(0x000100015758595a, 1).content(1, true, b"new shared"));
    let mut install = fixture.nand.try_install(&path, &fixture.keys).unwrap();
    while let Some(event) = install.next().await {
        if matches!(event.unwrap(), InstallEvent::File(_)) {
            break;
        }
    }
    drop(install);
    let mut bytes = std::fs::read(&path).unwrap();
    let offset = bytes.len() - 64;
    bytes[offset] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(
        fixture
            .nand
            .try_install(&path, &fixture.keys)
            .unwrap()
            .finish()
            .await
            .is_err()
    );
    assert_eq!(fixture.volume.artifacts().revision().unwrap(), revision);
    assert_eq!(fixture.read("sys/uid.sys"), uid);
    assert_eq!(fixture.read("shared1/content.map"), map);
    assert!(!fixture.volume.exists("title/00010001/5758595a/content"));
    assert!(!fixture.volume.is_dirty().unwrap());
}

#[tokio::test]
async fn interrupted_wii_publication_recovers_indexes_and_shared_files_together() {
    let fixture = WiiFixture::new().await;
    let a = fixture
        .install(Wad::new(0x0001000141424344, 1).content(1, true, b"common"))
        .await
        .unwrap();
    let lock = fixture.volume.index_path().with_extension("lock");
    std::fs::write(&lock, b"held").unwrap();
    assert!(
        fixture
            .install(Wad::new(0x000100015758595a, 1).content(1, true, b"second"))
            .await
            .is_err()
    );
    let id = fixture.volume.id();
    drop(fixture.nand);
    drop(fixture.volume);
    std::fs::remove_file(lock).unwrap();
    let platform = PlatformRepository::open(fixture.platform.path(), WiiNand::PLATFORM).unwrap();
    let volume = platform.volume(StorageKind::Nand, id);
    let nand = WiiNand::open(volume.clone()).await.unwrap();
    assert!(nand.is_complete(NandCompletion::Minimal));
    assert_eq!(volume.artifacts().summaries().await.unwrap().len(), 2);
    volume.artifacts().uninstall(&[a.id()]).await.unwrap();
    assert!(volume.exists("shared1/00000001.app"));
    assert!(!volume.exists("shared1/00000000.app"));
    assert!(!volume.is_dirty().unwrap());
}

#[tokio::test]
async fn wad_metadata_changes_and_duplicate_contents_cannot_publish() {
    let fixture = WiiFixture::new().await;
    let path = fixture.package(Wad::new(0x0001000141424344, 1));
    let install = fixture.nand.try_install(&path, &fixture.keys).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    let ticket_size = u32::from_be_bytes(bytes[16..20].try_into().unwrap()) as usize;
    let tmd_offset = 64 + ticket_size.next_multiple_of(64);
    bytes[tmd_offset + 0x18b] ^= 1;
    std::fs::write(path, bytes).unwrap();
    assert!(matches!(
        install.finish().await,
        Err(Error::IntegrityMatchFailed { .. })
    ));
    let duplicate = fixture.package(Wad::new(0x0001000141424344, 1).content(0, true, b"duplicate"));
    assert!(fixture.nand.try_install(&duplicate, &fixture.keys).is_err());
    assert!(
        fixture
            .volume
            .artifacts()
            .summaries()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(fixture.read("shared1/content.map").is_empty());
    assert!(!fixture.volume.is_dirty().unwrap());
}

#[tokio::test]
async fn shared_content_allocation_preserves_unowned_files() {
    let fixture = WiiFixture::new().await;
    fixture
        .volume
        .install_bytes(b"existing", "shared1/00000000.app")
        .unwrap();
    fixture
        .volume
        .set_label(Some("existing shared file"))
        .await
        .unwrap();
    assert!(matches!(
        fixture
            .install(Wad::new(0x0001000141424344, 1).content(1, true, b"different"))
            .await,
        Err(Error::HistoryConflict(_))
    ));
    assert_eq!(fixture.read("shared1/00000000.app"), b"existing");
    assert!(
        fixture
            .volume
            .artifacts()
            .summaries()
            .await
            .unwrap()
            .is_empty()
    );
    assert!(!fixture.volume.is_dirty().unwrap());
}

#[tokio::test]
async fn independent_repository_handles_publish_without_losing_other_nand_indexes() {
    let fixture = WiiFixture::new().await;
    let platform = PlatformRepository::open(fixture.platform.path(), WiiNand::PLATFORM).unwrap();
    let id = uuid::Uuid::new_v4();
    let volume = platform.volume(StorageKind::Nand, id);
    let nand = WiiNand::new_empty(volume.clone(), id, None).await.unwrap();
    for version in 1..=3 {
        let mut first = Wad::new(0x0001000141424344, version);
        let mut second = Wad::new(0x000100015758595a, version);
        for content in 1..=64 {
            first = first.content(content, false, &[version as u8; 256]);
            second = second.content(content, false, &[version as u8 + 10; 256]);
        }
        let a = fixture.package(first);
        let b = fixture.package(second);
        let a = fixture.nand.try_install(&a, &fixture.keys).unwrap();
        let b = nand.try_install(&b, &fixture.keys).unwrap();
        let (a, b) = tokio::join!(a.finish(), b.finish());
        a.unwrap();
        b.unwrap();
        assert!(!fixture.volume.is_dirty().unwrap());
        assert!(!volume.is_dirty().unwrap());
        assert_eq!(
            fixture.read("title/00010001/41424344/content/00000040.app"),
            vec![version as u8; 256]
        );
    }
    assert_eq!(
        volume
            .artifacts()
            .history("000100015758595a")
            .await
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        fixture
            .volume
            .artifacts()
            .history("0001000141424344")
            .await
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn deletion_resumes_after_index_failure_without_touching_another_nand() {
    let fixture = WiiFixture::new().await;
    fixture
        .install(Wad::new(0x0001000141424344, 1))
        .await
        .unwrap();
    let other_id = uuid::Uuid::new_v4();
    let other = fixture.platform.volume(StorageKind::Nand, other_id);
    WiiNand::new_empty(other.clone(), other_id, None)
        .await
        .unwrap();
    other.install_bytes(b"keep", "keep.bin").unwrap();
    other.set_label(Some("neighbor")).await.unwrap();
    let lock = fixture.volume.index_path().with_extension("lock");
    std::fs::write(&lock, b"held").unwrap();
    let id = fixture.volume.id();
    assert!(fixture.platform.delete(StorageKind::Nand, id).is_err());
    assert!(!fixture.platform.contains(StorageKind::Nand, id).unwrap());
    assert!(fixture.volume.checkout_path().exists());
    assert!(fixture.volume.recover().is_err());
    assert!(fixture.volume.set_label(Some("resurrect")).await.is_err());
    assert!(PlatformRepository::open(fixture.platform.path(), WiiNand::PLATFORM).is_err());
    std::fs::remove_file(lock).unwrap();
    let reopened = PlatformRepository::open(fixture.platform.path(), WiiNand::PLATFORM).unwrap();
    assert!(!fixture.volume.checkout_path().exists());
    assert_eq!(reopened.ids(StorageKind::Nand).unwrap(), vec![other_id]);
    assert!(!other.is_dirty().unwrap());
    reopened.delete(StorageKind::Nand, id).unwrap();
    assert!(!other.is_dirty().unwrap());
}

#[tokio::test]
async fn deletion_intent_survives_reference_failure_and_overrides_install_recovery() {
    let fixture = WiiFixture::new().await;
    let lock = fixture.volume.index_path().with_extension("lock");
    std::fs::write(&lock, b"held").unwrap();
    assert!(
        fixture
            .install(Wad::new(0x0001000141424344, 1))
            .await
            .is_err()
    );
    let id = fixture.volume.id();
    let reference_lock = fixture
        .platform
        .path()
        .join(format!(".git/refs/consoles/nands/v2/{}.lock", id.simple()));
    std::fs::write(&reference_lock, b"held").unwrap();
    assert!(fixture.platform.delete(StorageKind::Nand, id).is_err());
    assert!(fixture.platform.contains(StorageKind::Nand, id).unwrap());
    assert!(fixture.volume.recover().is_err());
    std::fs::remove_file(reference_lock).unwrap();
    std::fs::remove_file(lock).unwrap();
    let reopened = PlatformRepository::open(fixture.platform.path(), WiiNand::PLATFORM).unwrap();
    assert!(reopened.ids(StorageKind::Nand).unwrap().is_empty());
    assert!(!fixture.volume.checkout_path().exists());
    assert!(
        WiiNand::open(reopened.volume(StorageKind::Nand, id))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn catalog_reconciles_uninstalled_titles_and_interrupted_volume_deletion() {
    use polymer_library::{
        GameCatalog, IdentifiedLaunchable, ImportOrigin, LaunchableUri, StorageManager,
    };
    use std::sync::Arc;
    let fixture = WiiFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let manager = StorageManager::new(directory.path().join("storage"));
    let nand = manager
        .create(StorageKind::Nand, WiiNand::PLATFORM, None)
        .await
        .unwrap();
    let id = nand.metadata().await.unwrap().id;
    let title = manager
        .install(
            WiiNand::PLATFORM,
            id,
            &fixture.package(Wad::new(0x0001000141424344, 1)),
            |_| {},
        )
        .await
        .unwrap();
    let catalog = GameCatalog::open(
        directory.path().join("library.db"),
        Arc::new(object_store::memory::InMemory::new()),
    )
    .await
    .unwrap();
    let installed = LaunchableUri::artifact(WiiNand::PLATFORM.into(), id, title.id.clone());
    let other_id = uuid::Uuid::new_v4();
    let other = LaunchableUri::artifact(WiiNand::PLATFORM.into(), other_id, title.id.clone());
    let file = LaunchableUri::file(directory.path().join("copy.wad")).unwrap();
    for path in [installed.clone(), other.clone(), file.clone()] {
        catalog
            .import_package(
                IdentifiedLaunchable {
                    manifest: None,
                    path,
                    entrypoint: None,
                    package: None,
                    binary_blake3: [42; 32],
                    serial: None,
                    title_id: None,
                    disc_number: None,
                    requires_license: false,
                    parent_binary_id: None,
                    platform: WiiNand::PLATFORM.into(),
                },
                Vec::new(),
                ImportOrigin::Manual,
            )
            .await
            .unwrap();
    }
    nand.as_nand()
        .unwrap()
        .uninstall(&[&title.id])
        .await
        .unwrap();
    catalog
        .retain_artifacts(
            WiiNand::PLATFORM,
            id,
            &manager
                .inspect(WiiNand::PLATFORM, id)
                .await
                .unwrap()
                .artifacts,
        )
        .await
        .unwrap();
    assert!(catalog.launchable_at(&installed).await.unwrap().is_none());
    assert!(catalog.launchable_at(&other).await.unwrap().is_some());
    assert!(catalog.launchable_at(&file).await.unwrap().is_some());
    assert_eq!(catalog.games().await.unwrap()[0].launchables.len(), 2);
    let input = IdentifiedLaunchable {
        manifest: None,
        path: installed.clone(),
        entrypoint: None,
        package: None,
        binary_blake3: [43; 32],
        serial: None,
        title_id: None,
        disc_number: None,
        requires_license: false,
        parent_binary_id: None,
        platform: WiiNand::PLATFORM.into(),
    };
    catalog
        .import_package(input, Vec::new(), ImportOrigin::Manual)
        .await
        .unwrap();
    let platform =
        PlatformRepository::open(manager.platform_path(WiiNand::PLATFORM), WiiNand::PLATFORM)
            .unwrap();
    let volume = platform.volume(StorageKind::Nand, id);
    let lock = volume.index_path().with_extension("lock");
    std::fs::write(&lock, b"held").unwrap();
    assert!(
        manager
            .delete(StorageKind::Nand, WiiNand::PLATFORM, id)
            .await
            .is_err()
    );
    std::fs::remove_file(lock).unwrap();
    catalog.reconcile_storage(&manager).await.unwrap();
    assert!(!volume.checkout_path().exists());
    assert!(catalog.launchable_at(&installed).await.unwrap().is_none());
    assert!(catalog.launchable_at(&other).await.unwrap().is_none());
    let games = catalog.games().await.unwrap();
    assert_eq!(games.len(), 1);
    assert_eq!(games[0].launchables.len(), 1);
    assert_eq!(games[0].launchables[0].identified.path, file);
    catalog.reconcile_storage(&manager).await.unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn deletion_retries_checkout_cleanup_after_an_open_file_is_released() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = WiiFixture::new().await;
    fixture
        .install(Wad::new(0x0001000141424344, 1))
        .await
        .unwrap();
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(
            fixture
                .volume
                .checkout_path()
                .join("title/00010001/41424344/content/00000000.app"),
        )
        .unwrap();
    let id = fixture.volume.id();
    assert!(fixture.platform.delete(StorageKind::Nand, id).is_err());
    assert!(!fixture.platform.contains(StorageKind::Nand, id).unwrap());
    drop(held);
    fixture.platform.delete(StorageKind::Nand, id).unwrap();
    assert!(!fixture.volume.checkout_path().exists());
    assert!(fixture.platform.ids(StorageKind::Nand).unwrap().is_empty());
}
