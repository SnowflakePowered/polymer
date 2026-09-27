use polymer_consolestorage::savegame::SaveDepotId;
use polymer_library::keys::KeyStore;
use polymer_library::{ConsoleStorage, StorageKind, StorageManager};

const SD: &str =
    "sdmc/Nintendo 3DS/00000000000000000000000000000000/00000000000000000000000000000000";

#[tokio::test]
async fn emulator_writes_reach_saves_but_never_the_nand() {
    let directory = tempfile::tempdir().unwrap();
    let manager = StorageManager::new(directory.path());
    let store = KeyStore::open(directory.path().join("keys")).await.unwrap();
    let keys = store.for_platform("NINTENDO_3DS").unwrap();
    let ConsoleStorage::Nand(nand) = manager
        .create(StorageKind::Nand, "NINTENDO_3DS", None)
        .await
        .unwrap()
    else {
        panic!("expected NAND")
    };
    let ConsoleStorage::SaveRepo(saves) = manager
        .create(StorageKind::SaveRepo, "NINTENDO_3DS", None)
        .await
        .unwrap()
    else {
        panic!("expected save repository")
    };
    let composed = nand.compose(Some(saves.clone()), &keys).await.unwrap();

    let title = format!("{SD}/title/00040000/00033500");
    for directory in [
        format!("{SD}/title/00040000"),
        title.clone(),
        format!("{title}/data/00000001"),
    ] {
        composed.create_dir_all(&directory).unwrap();
    }
    composed
        .create(&format!("{title}/data/00000001/system.dat"))
        .unwrap()
        .write_all_at(0, b"save")
        .unwrap();
    composed
        .create("nand/dbs/title.db")
        .unwrap()
        .write_all_at(0, b"database")
        .unwrap();
    composed.remove("nand/dbs").unwrap_err();

    assert!(!nand.root().is_dirty().unwrap());
    assert!(
        saves
            .enumerate_saves()
            .any(|save| save.id == SaveDepotId(0x0004000000033500))
    );
    assert!(composed.exists(&title));
    assert!(composed.exists("nand/dbs/title.db"));
    assert!(!nand.root().exists("nand/dbs/title.db"));
}
