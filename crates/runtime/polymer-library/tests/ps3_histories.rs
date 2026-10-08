mod ps3_support;

use polymer_consolestorage::installable::InstallEvent;
use polymer_consolestorage::nand::VirtualNand;
use polymer_consolestorage::{Error, Storage, VolumeStorage};
use ps3_support::{Package, Ps3Fixture};

#[tokio::test]
async fn interleaved_packages_replay_in_original_order() {
    let fixture = Ps3Fixture::new().await;
    let base = fixture
        .install(Package::base("01.00").file("shared", b"base", true))
        .await
        .unwrap();
    let dlc = fixture
        .install(Package::dlc("DLC0000000000001").file("shared", b"dlc", true))
        .await
        .unwrap();
    let update = fixture
        .install(Package::update("01.01").file("new", b"update", true))
        .await
        .unwrap();
    assert_eq!(fixture.read("shared"), b"dlc");
    assert_eq!(fixture.read("new"), b"update");
    let registry = fixture.volume.artifacts();
    let history = registry.history(update.id()).await.unwrap();
    assert_eq!(history.len(), 2);
    registry
        .restore(base.id(), &history[1].revision)
        .await
        .unwrap();
    assert_eq!(fixture.read("shared"), b"dlc");
    assert!(!fixture.volume.exists("dev_hdd0/game/NPUB00001/new"));
    fixture
        .install(Package::update("01.02").file("new", b"second", true))
        .await
        .unwrap();
    registry.uninstall(&[dlc.id()]).await.unwrap();
    assert_eq!(fixture.read("shared"), b"base");
    assert_eq!(fixture.read("new"), b"second");
    assert!(!fixture.volume.is_dirty().unwrap());
}

#[tokio::test]
async fn skipped_files_become_visible_after_their_blocker_is_removed() {
    let fixture = Ps3Fixture::new().await;
    fixture.install(Package::base("01.00")).await.unwrap();
    let first = fixture
        .install(Package::dlc("DLC0000000000001").file("shared", b"first", true))
        .await
        .unwrap();
    let second = fixture
        .install(Package::dlc("DLC0000000000002").file("shared", b"second", false))
        .await
        .unwrap();
    assert_eq!(second.version(), None);
    assert_eq!(fixture.read("shared"), b"first");
    fixture
        .volume
        .artifacts()
        .uninstall(&[first.id()])
        .await
        .unwrap();
    assert_eq!(fixture.read("shared"), b"second");
    assert!(
        fixture
            .volume
            .artifacts()
            .contains(second.id())
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn target_versions_gate_installation_and_rollback() {
    let fixture = Ps3Fixture::new().await;
    let base = fixture.install(Package::base("01.00")).await.unwrap();
    let update = fixture.install(Package::update("01.01")).await.unwrap();
    let mut dlc = Package::dlc("DLC0000000000001").file("extra", b"dlc", true);
    dlc.version = Some("01.00");
    dlc.target = Some("01.01");
    dlc.sfo_overwrite = false;
    let dlc = fixture.install(dlc).await.unwrap();
    let sfo = fixture.read("PARAM.SFO");
    let registry = fixture.volume.artifacts();
    let history = registry.history(base.id()).await.unwrap();
    assert!(matches!(
        registry.restore(base.id(), &history[1].revision).await,
        Err(Error::AffectedDepots { .. })
    ));
    assert_eq!(
        registry.affected(&[update.id()]).await.unwrap(),
        vec![dlc.id()]
    );
    let mut invalid = Package::update("01.02");
    invalid.target = Some("01.00");
    assert!(matches!(
        fixture.install(invalid).await,
        Err(Error::AffectedDepots { .. })
    ));
    assert_eq!(fixture.read("PARAM.SFO"), sfo);
    registry.uninstall(&[update.id(), dlc.id()]).await.unwrap();
    assert!(registry.contains(base.id()).await.unwrap());
    assert!(!fixture.volume.exists("dev_hdd0/game/NPUB00001/extra"));
}

#[tokio::test]
async fn incremental_updates_and_firmware_have_distinct_history_rules() {
    let fixture = Ps3Fixture::new().await;
    let base = fixture.install(Package::base("01.00")).await.unwrap();
    let update = fixture
        .install(Package::update("01.01").file("first", b"one", true))
        .await
        .unwrap();
    fixture
        .install(Package::update("01.02").file("second", b"two", true))
        .await
        .unwrap();
    assert_eq!(fixture.read("first"), b"one");
    fixture.firmware("4.91", false).await;
    assert_eq!(fixture.read("second"), b"two");
    assert!(!fixture.volume.exists("dev_flash/obsolete"));
    let registry = fixture.volume.artifacts();
    assert_eq!(
        registry.affected(&[base.id()]).await.unwrap(),
        vec![update.id()]
    );
    let affected = registry.affected(&["ps3:firmware"]).await.unwrap();
    assert!(affected.contains(&base.id().to_owned()) && affected.contains(&update.id().to_owned()));
    let history = registry.history("ps3:firmware").await.unwrap();
    registry
        .restore("ps3:firmware", &history[1].revision)
        .await
        .unwrap();
    assert!(fixture.volume.exists("dev_flash/obsolete"));
    assert_eq!(fixture.read("first"), b"one");
}

#[tokio::test]
async fn distinct_update_ids_report_every_truncated_package() {
    let fixture = Ps3Fixture::new().await;
    fixture.install(Package::base("01.00")).await.unwrap();
    let first = fixture
        .install(Package::update("01.01").file("first", b"one", true))
        .await
        .unwrap();
    let mut next = Package::update("01.02").file("second", b"two", true);
    next.suffix = "NEXT000000000000";
    let second = fixture.install(next).await.unwrap();
    let registry = fixture.volume.artifacts();
    assert_eq!(
        registry.affected(&[first.id()]).await.unwrap(),
        vec![second.id()]
    );
    assert!(registry.uninstall(&[first.id()]).await.is_err());
    assert_eq!(fixture.read("second"), b"two");
    registry
        .uninstall(&[first.id(), second.id()])
        .await
        .unwrap();
    assert!(!fixture.volume.exists("dev_hdd0/game/NPUB00001/first"));
    assert!(!fixture.volume.exists("dev_hdd0/game/NPUB00001/second"));
}

#[tokio::test]
async fn corrupt_and_cancelled_packages_do_not_publish() {
    let fixture = Ps3Fixture::new().await;
    let base = fixture.install(Package::base("01.00")).await.unwrap();
    let revision = fixture.volume.artifacts().revision().unwrap();
    let path = fixture.package(Package::update("01.01").file("extra", b"new", true));
    let mut install = fixture.nand.try_install(&path, &fixture.keys).unwrap();
    while let Some(event) = install.next().await {
        if matches!(event.unwrap(), InstallEvent::File(_)) {
            break;
        }
    }
    drop(install);
    assert_eq!(fixture.volume.artifacts().revision().unwrap(), revision);
    let install = fixture.nand.try_install(&path, &fixture.keys).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0xb0] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(matches!(
        install.finish().await,
        Err(Error::IntegrityMatchFailed { .. })
    ));
    assert_eq!(fixture.volume.artifacts().revision().unwrap(), revision);
    assert_eq!(
        fixture
            .volume
            .artifacts()
            .history(base.id())
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(!fixture.volume.exists("dev_hdd0/game/NPUB00001/extra"));
    assert!(!fixture.volume.is_dirty().unwrap());
}

#[tokio::test]
async fn interrupted_checkout_recovers_the_composed_package_selection() {
    let fixture = Ps3Fixture::new().await;
    fixture
        .install(Package::base("01.00").file("shared", b"base", true))
        .await
        .unwrap();
    fixture
        .install(Package::dlc("DLC0000000000001").file("shared", b"dlc", true))
        .await
        .unwrap();
    let lock = fixture.volume.index_path().with_extension("lock");
    std::fs::write(&lock, b"held").unwrap();
    assert!(
        fixture
            .install(Package::update("01.01").file("new", b"update", true))
            .await
            .is_err()
    );
    std::fs::remove_file(lock).unwrap();
    fixture.volume.recover().unwrap();
    assert_eq!(fixture.read("shared"), b"dlc");
    assert_eq!(fixture.read("new"), b"update");
    assert_eq!(
        fixture.volume.artifacts().summaries().await.unwrap().len(),
        4
    );
    assert!(!fixture.volume.is_dirty().unwrap());
}
