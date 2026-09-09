use std::collections::BTreeSet;
use std::io::{Cursor, Read};

use polymer_consolestorage::savegame::Package;
use polymer_consolestorage::{ConsoleStorage, PlatformRepository, Save, Storage, StorageKind};
use uuid::Uuid;

#[tokio::test]
async fn mounted_families_restore_all_slots_and_survive_reopening() {
    let directory = tempfile::tempdir().unwrap();
    let platform = PlatformRepository::open(directory.path().join("SONY_PS3"), "SONY_PS3").unwrap();
    let nand_id = Uuid::now_v7();
    let save_id = Uuid::now_v7();
    let license = "dev_hdd0/home/00000001/exdata/license.rap";
    let nand_storage = platform.volume(StorageKind::Nand, nand_id);
    // Seed the fixture before branding commits the initial NAND contents.
    nand_storage
        .install_file(Box::new(Cursor::new(b"system license")), license)
        .await
        .unwrap();
    let ConsoleStorage::Nand(nand) = ConsoleStorage::create(
        StorageKind::Nand,
        "SONY_PS3",
        nand_storage,
        nand_id,
        Some("System".to_owned()),
    )
    .await
    .unwrap() else {
        panic!("expected PS3 system volume");
    };
    let ConsoleStorage::SaveRepo(saves) = ConsoleStorage::create(
        StorageKind::SaveRepo,
        "SONY_PS3",
        platform.volume(StorageKind::SaveRepo, save_id),
        save_id,
        Some("Player".to_owned()),
    )
    .await
    .unwrap() else {
        panic!("expected PS3 save repository");
    };
    assert!(!saves.exists("dev_hdd0/home/00000001/exdata"));
    // A misplaced profile license must never shadow the system NAND's license.
    saves
        .install_file(
            Box::new(Cursor::new(b"profile license")),
            "dev_hdd0/home/00000001/exdata/license.rap",
        )
        .await
        .unwrap();
    let mounted = nand.compose(saves.clone()).unwrap();
    assert!(!nand.permissions(license).can_write());
    let root = "dev_hdd0/home/00000001/savedata";
    let trophy = "dev_hdd0/home/00000001/trophy/NPWR00001_00/TROPUSR.DAT";
    mounted
        .install_file(Box::new(Cursor::new(b"trophy profile")), trophy)
        .await
        .unwrap();
    assert!(saves.exists("dev_hdd0/home/00000001/trophy/NPWR00001_00/TROPUSR.DAT"));
    assert!(saves.exists("dev_hdd0/home/00000001/localusername"));
    assert!(saves.exists("dev_hdd0/home/00000001/savedata"));
    let names = [
        "BLES00001-nick01save01",
        "BLES00001-nick01save02",
        "BLUS00002-autosave",
    ];
    for (index, name) in names.iter().enumerate() {
        mounted
            .install_file(
                Box::new(Cursor::new(vec![index as u8; 8])),
                &format!("{root}/{name}/DATA.BIN"),
            )
            .await
            .unwrap();
    }
    let title = saves.infer_save_depot(&Package::new(Some("BLES00001".to_owned()), None));
    let other = saves.infer_save_depot(&Package::new(Some("BLUS00002".to_owned()), None));
    assert_eq!(
        saves
            .enumerate_saves()
            .map(|save| save.id)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([title, other])
    );
    saves.checkpoint_saves().await.unwrap();
    let baseline = Save::new(saves.as_ref(), title).history().unwrap()[0].id;
    let other_baseline = Save::new(saves.as_ref(), other).history().unwrap()[0].id;
    for name in &names[..2] {
        mounted
            .open(&format!("{root}/{name}/DATA.BIN"), true)
            .unwrap()
            .write_all_at(0, b"modified")
            .unwrap();
    }
    let added = format!("{root}/BLES00001-extra/DATA.BIN");
    mounted
        .create(&added)
        .unwrap()
        .write_all_at(0, b"new slot")
        .unwrap();
    saves.checkpoint_saves().await.unwrap();
    Save::new(saves.as_ref(), title)
        .restore(baseline)
        .await
        .unwrap();
    assert!(!mounted.exists(&added));
    assert_eq!(
        Save::new(saves.as_ref(), other).history().unwrap()[0].id,
        other_baseline
    );
    mounted.finish("").unwrap();
    drop(mounted);
    drop(saves);
    drop(nand);
    drop(platform);

    let platform = PlatformRepository::open(directory.path().join("SONY_PS3"), "SONY_PS3").unwrap();
    let ConsoleStorage::Nand(nand) = ConsoleStorage::open(
        StorageKind::Nand,
        platform.volume(StorageKind::Nand, nand_id),
    )
    .await
    .unwrap() else {
        panic!("expected reopened system volume");
    };
    let ConsoleStorage::SaveRepo(saves) = ConsoleStorage::open(
        StorageKind::SaveRepo,
        platform.volume(StorageKind::SaveRepo, save_id),
    )
    .await
    .unwrap() else {
        panic!("expected reopened save repository");
    };
    let mounted = nand.compose(saves.clone()).unwrap();
    let mut license_bytes = Vec::new();
    mounted
        .open_read(license)
        .unwrap()
        .read_to_end(&mut license_bytes)
        .unwrap();
    assert_eq!(license_bytes, b"system license");
    let mut trophy_bytes = Vec::new();
    mounted
        .open_read(trophy)
        .unwrap()
        .read_to_end(&mut trophy_bytes)
        .unwrap();
    assert_eq!(trophy_bytes, b"trophy profile");
    assert!(!mounted.exists("save-families"));
    assert!(!mounted.exists("profile"));
    assert_eq!(
        mounted
            .read_dir(root, false)
            .unwrap()
            .map(|entry| entry.name)
            .collect::<BTreeSet<_>>(),
        names
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
    );
    for (index, name) in names.iter().enumerate() {
        let mut bytes = Vec::new();
        mounted
            .open_read(&format!("{root}/{name}/DATA.BIN"))
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, vec![index as u8; 8]);
    }
    assert_eq!(Save::new(saves.as_ref(), title).history().unwrap().len(), 3);
    mounted.finish("").unwrap();
}
