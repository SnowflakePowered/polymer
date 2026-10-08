mod wii_support;

use std::io::Read;
use std::sync::{Arc, Barrier};

use polymer_consolestorage::impls::wii::WiiNand;
use polymer_consolestorage::nand::VirtualNand;
use polymer_consolestorage::{
    MountableStorage, SaveRepo, StorageFile, StorageKind, StorageReader, VersionedStorage,
};
use uuid::Uuid;
use wii_support::WiiFixture;

const T: &str = "tmp/t";
const OTHER: &str = "tmp/other";
const D: &str = "title/00010001/41424344/data/d";
const DF: &str = "title/00010001/41424344/data/d/f";

struct Fixture {
    storage: Arc<dyn MountableStorage>,
    _saves: Arc<dyn SaveRepo>,
    _wii: WiiFixture,
}

impl Fixture {
    async fn new() -> Self {
        let wii = WiiFixture::new().await;
        let id = Uuid::new_v4();
        let volume = wii.platform.volume(StorageKind::SaveRepo, id);
        let saves = WiiNand::new_save_repo(VersionedStorage::new(volume).unwrap(), id, None)
            .await
            .unwrap();
        saves
            .create(DF)
            .unwrap()
            .write_all_at(0, b"displaced")
            .unwrap();
        let storage = wii
            .nand
            .compose(Some(saves.clone()), Arc::new(wii.keys.clone()))
            .await
            .unwrap();
        storage
            .create(T)
            .unwrap()
            .write_all_at(0, b"source")
            .unwrap();
        Self {
            storage,
            _saves: saves,
            _wii: wii,
        }
    }

    fn bytes(file: Arc<dyn StorageFile>) -> Vec<u8> {
        let mut bytes = Vec::new();
        StorageReader::new(file).read_to_end(&mut bytes).unwrap();
        bytes
    }
}

#[tokio::test]
async fn handles_follow_cross_layer_round_trip_and_preserve_access() {
    let fixture = Fixture::new().await;
    let storage = &fixture.storage;
    let writer = storage.open(T, true).unwrap();
    let reader = storage.open(T, false).unwrap();
    let displaced = storage.open(DF, true).unwrap();
    storage.rename(T, DF).unwrap();
    writer.write_all_at(0, b"moved!").unwrap();
    assert_eq!(Fixture::bytes(reader.clone()), b"moved!");
    assert!(reader.write_at(0, b"bad").is_err());
    assert!(reader.set_len(0).is_err());
    assert_eq!(Fixture::bytes(displaced.clone()), b"displaced");
    displaced.write_all_at(0, b"detached!").unwrap();
    assert_eq!(Fixture::bytes(storage.open(DF, false).unwrap()), b"moved!");
    storage.rename(DF, T).unwrap();
    writer.write_all_at(0, b"return").unwrap();
    assert_eq!(Fixture::bytes(reader), b"return");
    assert_eq!(Fixture::bytes(storage.open(T, false).unwrap()), b"return");
}

#[tokio::test]
async fn directory_rename_rekeys_children_for_subsequent_cross_layer_move() {
    let fixture = Fixture::new().await;
    let storage = &fixture.storage;
    let original = storage.open(DF, true).unwrap();
    storage.rename(DF, "tmp/d/f").unwrap();
    storage.rename("tmp/d", "tmp/e").unwrap();
    storage
        .create("tmp/d/f")
        .unwrap()
        .write_all_at(0, b"unrelated")
        .unwrap();
    let reopened = storage.open("tmp/e/f", true).unwrap();
    storage.rename("tmp/e/f", DF).unwrap();
    reopened.write_all_at(0, b"reopened!").unwrap();
    assert_eq!(Fixture::bytes(original.clone()), b"reopened!");
    original.write_all_at(0, b"original!").unwrap();
    assert_eq!(
        Fixture::bytes(storage.open(DF, false).unwrap()),
        b"original!"
    );
    assert_eq!(
        Fixture::bytes(storage.open("tmp/d/f", false).unwrap()),
        b"unrelated"
    );
}

#[tokio::test]
async fn atomic_install_detaches_redirected_handles_and_binds_new_opens() {
    let fixture = Fixture::new().await;
    let storage = &fixture.storage;
    let old = storage.open(T, true).unwrap();
    storage.rename(T, DF).unwrap();
    let result = storage
        .install_file(Box::new(std::io::Cursor::new(b"replacement")), DF)
        .await
        .unwrap();
    assert_eq!(result.byte_length, 11);
    assert_eq!(result.blake3, *blake3::hash(b"replacement").as_bytes());
    old.write_all_at(0, b"orphan").unwrap();
    assert_eq!(Fixture::bytes(old.clone()), b"orphan");
    assert_eq!(
        Fixture::bytes(storage.open(DF, false).unwrap()),
        b"replacement"
    );
    let new = storage.open(DF, true).unwrap();
    storage.rename(DF, T).unwrap();
    new.set_len(3).unwrap();
    assert_eq!(Fixture::bytes(storage.open(T, false).unwrap()), b"rep");
    assert_eq!(Fixture::bytes(old), b"orphan");
}

#[tokio::test]
async fn remove_and_recreate_do_not_reuse_a_live_binding() {
    let fixture = Fixture::new().await;
    let storage = &fixture.storage;
    let old = storage.open(T, true).unwrap();
    storage.rename(T, DF).unwrap();
    storage.remove(DF).unwrap();
    let replacement = storage.create(DF).unwrap();
    replacement.write_all_at(0, b"new").unwrap();
    storage.rename(DF, T).unwrap();
    old.write_all_at(0, b"orphan").unwrap();
    assert_eq!(Fixture::bytes(replacement), b"new");
    assert_eq!(Fixture::bytes(storage.open(T, false).unwrap()), b"new");
}

#[tokio::test]
async fn create_truncates_the_object_seen_by_existing_handles() {
    let fixture = Fixture::new().await;
    let storage = &fixture.storage;
    let moved = storage.open(T, true).unwrap();
    storage.rename(T, DF).unwrap();
    let created = storage.create(DF).unwrap();
    assert_eq!(moved.metadata().unwrap().len, 0);
    created.write_all_at(0, b"new contents").unwrap();
    assert_eq!(Fixture::bytes(moved.clone()), b"new contents");
    moved.set_len(3).unwrap();
    assert_eq!(Fixture::bytes(created), b"new");
    storage.rename(DF, T).unwrap();
    storage
        .create(T)
        .unwrap()
        .write_all_at(0, b"scratch")
        .unwrap();
    assert_eq!(Fixture::bytes(moved), b"scratch");
}

#[tokio::test]
async fn concurrent_acknowledged_writes_survive_cross_layer_copy() {
    let fixture = Fixture::new().await;
    let storage = fixture.storage.clone();
    let writer = storage.open(T, true).unwrap();
    writer.set_len(4 * 1024 * 1024).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let start = barrier.clone();
    let writing = std::thread::spawn(move || {
        start.wait();
        for token in 1..=256_u64 {
            writer
                .write_all_at(token * 8, &token.to_le_bytes())
                .unwrap();
            std::thread::yield_now();
        }
    });
    barrier.wait();
    storage.rename(T, DF).unwrap();
    writing.join().unwrap();
    let result = storage.open(DF, false).unwrap();
    for token in 1..=256_u64 {
        let mut bytes = [0; 8];
        assert_eq!(result.read_at(token * 8, &mut bytes).unwrap(), 8);
        assert_eq!(u64::from_le_bytes(bytes), token);
    }
}

#[tokio::test]
async fn slow_install_input_does_not_block_guest_io() {
    use tokio::io::AsyncWriteExt;
    let fixture = Fixture::new().await;
    let storage = &fixture.storage;
    let (mut producer, input) = tokio::io::duplex(8);
    let installation = storage.install_file(Box::new(input), DF);
    let producing = async {
        producer.write_all(b"first").await.unwrap();
        storage.rename(T, OTHER).unwrap();
        storage
            .open(OTHER, true)
            .unwrap()
            .write_all_at(0, b"during")
            .unwrap();
        producer.write_all(b"second").await.unwrap();
        producer.shutdown().await.unwrap();
    };
    let (result, ()) = tokio::join!(installation, producing);
    result.unwrap();
    assert_eq!(
        Fixture::bytes(storage.open(DF, false).unwrap()),
        b"firstsecond"
    );
    assert_eq!(
        Fixture::bytes(storage.open(OTHER, false).unwrap()),
        b"during"
    );
}

#[tokio::test]
async fn installation_preserves_old_handles_and_rejects_directories() {
    let fixture = Fixture::new().await;
    let storage = &fixture.storage;
    let old = storage.open(T, true).unwrap();
    storage
        .install_file(Box::new(std::io::Cursor::new(b"new scratch")), T)
        .await
        .unwrap();
    old.write_all_at(0, b"orphan").unwrap();
    assert_eq!(
        Fixture::bytes(storage.open(T, false).unwrap()),
        b"new scratch"
    );
    assert_eq!(Fixture::bytes(old), b"orphan");
    let result = storage
        .install_file(Box::new(std::io::Cursor::new(b"invalid")), D)
        .await;
    assert!(result.is_err());
    assert_eq!(
        Fixture::bytes(storage.open(DF, false).unwrap()),
        b"displaced"
    );
}
