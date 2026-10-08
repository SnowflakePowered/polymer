mod wii_support;

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use polymer_consolestorage::impls::wii::WiiNand;
use polymer_consolestorage::nand::VirtualNand;
use polymer_consolestorage::{
    MountableStorage, SaveRepo, Storage, StorageFile, StorageKind, StorageReader, VersionedStorage,
    VolumeStorage,
};
use uuid::Uuid;
use wii_support::WiiFixture;

const SAVE: &str = "title/00010001/41424344/data/save.bin";
const TEMP: &str = "tmp/save.tmp";

struct Fixture {
    wii: WiiFixture,
    id: Uuid,
}

impl Fixture {
    async fn new() -> Self {
        Self {
            wii: WiiFixture::new().await,
            id: Uuid::new_v4(),
        }
    }

    async fn saves(&self, create: bool) -> Arc<dyn SaveRepo> {
        let volume = self.wii.platform.volume(StorageKind::SaveRepo, self.id);
        let storage = VersionedStorage::new(volume).unwrap();
        if create {
            let saves = WiiNand::new_save_repo(storage, self.id, None)
                .await
                .unwrap();
            saves
                .create(SAVE)
                .unwrap()
                .write_all_at(0, b"original")
                .unwrap();
            saves.set_label(Some("baseline")).await.unwrap();
            saves
        } else {
            WiiNand::open_save_repo(storage).await.unwrap()
        }
    }

    async fn mount(&self, saves: Arc<dyn SaveRepo>) -> Arc<dyn MountableStorage> {
        self.wii
            .nand
            .compose(Some(saves), Arc::new(self.wii.keys.clone()))
            .await
            .unwrap()
    }

    fn lock_profile(&self) -> PathBuf {
        let path = self.wii.platform.path().join(format!(
            ".git/refs/consoles/profiles/v2/{}.lock",
            self.id.simple()
        ));
        fs::write(&path, b"held").unwrap();
        path
    }

    fn journal_directory(&self) -> PathBuf {
        self.wii
            .platform
            .path()
            .join(".git/polymer/operations")
            .join(self.id.simple().to_string())
    }

    fn bytes(file: Arc<dyn StorageFile>) -> Vec<u8> {
        let mut bytes = Vec::new();
        StorageReader::new(file).read_to_end(&mut bytes).unwrap();
        bytes
    }
}

#[tokio::test]
async fn invalid_destination_does_not_block_a_subsequent_rename() {
    let fixture = Fixture::new().await;
    let mounted = fixture.mount(fixture.saves(true).await).await;
    mounted
        .create(TEMP)
        .unwrap()
        .write_all_at(0, b"replacement")
        .unwrap();
    assert!(
        mounted
            .rename(TEMP, "title/00010001/41424344/data")
            .is_err()
    );
    assert!(mounted.rename(TEMP, &format!("{SAVE}/child")).is_err());
    assert_eq!(
        Fixture::bytes(mounted.open(SAVE, false).unwrap()),
        b"original"
    );
    mounted.rename(TEMP, SAVE).unwrap();
    assert_eq!(
        Fixture::bytes(mounted.open(SAVE, false).unwrap()),
        b"replacement"
    );
}

#[tokio::test]
async fn failed_commit_blocks_io_and_retries_without_losing_live_bindings() {
    let fixture = Fixture::new().await;
    let saves = fixture.saves(true).await;
    let mounted = fixture.mount(saves.clone()).await;
    let displaced = mounted.open(SAVE, false).unwrap();
    let source = mounted.create(TEMP).unwrap();
    source.write_all_at(0, b"replacement").unwrap();
    let lock = fixture.lock_profile();
    assert!(mounted.rename(TEMP, SAVE).is_err());
    assert!(source.write_at(0, b"lost").is_err());
    assert!(source.metadata().is_err());
    assert!(mounted.open(SAVE, true).is_err());
    assert!(mounted.create("tmp/other").is_err());
    assert!(mounted.remove(SAVE).is_err());
    assert!(saves.checkpoint_saves().await.is_err());
    assert!(mounted.finish("").is_err());
    fs::remove_file(lock).unwrap();
    mounted.finish("").unwrap();
    assert_eq!(Fixture::bytes(source.clone()), b"replacement");
    assert_eq!(Fixture::bytes(displaced), b"original");
    source.write_all_at(0, b"newer").unwrap();
    source.set_len(5).unwrap();
    mounted.sync(SAVE).unwrap();
    drop(source);
    drop(mounted);
    drop(saves);
    let saves = fixture.saves(false).await;
    let mounted = fixture.mount(saves.clone()).await;
    assert_eq!(Fixture::bytes(mounted.open(SAVE, false).unwrap()), b"newer");
    saves.checkpoint_saves().await.unwrap();
    assert!(
        mounted
            .read_dir("", true)
            .unwrap()
            .all(|entry| !entry.name.contains(".polymer-"))
    );
}

#[tokio::test]
async fn remount_replays_publication_and_keeps_a_regenerated_volatile_source() {
    let fixture = Fixture::new().await;
    fixture
        .wii
        .volume
        .install_bytes(b"regenerated base", TEMP)
        .unwrap();
    fixture
        .wii
        .volume
        .set_label(Some("base source"))
        .await
        .unwrap();
    let lock;
    {
        let saves = fixture.saves(true).await;
        let mounted = fixture.mount(saves).await;
        mounted
            .create(TEMP)
            .unwrap()
            .write_all_at(0, b"guest edit")
            .unwrap();
        lock = fixture.lock_profile();
        assert!(mounted.rename(TEMP, SAVE).is_err());
    }
    let saves = fixture.saves(false).await;
    assert!(saves.checkpoint_saves().await.is_err());
    assert!(
        fixture
            .wii
            .nand
            .compose(Some(saves.clone()), Arc::new(fixture.wii.keys.clone()))
            .await
            .is_err()
    );
    fs::remove_file(lock).unwrap();
    let mounted = fixture.mount(saves).await;
    assert_eq!(
        Fixture::bytes(mounted.open(SAVE, false).unwrap()),
        b"guest edit"
    );
    assert_eq!(
        Fixture::bytes(mounted.open(TEMP, false).unwrap()),
        b"regenerated base"
    );
}

#[tokio::test]
async fn recovery_persists_source_removal_without_making_scratch_permanent() {
    let fixture = Fixture::new().await;
    let lock;
    {
        let mounted = fixture.mount(fixture.saves(true).await).await;
        lock = fixture.lock_profile();
        assert!(mounted.rename(SAVE, TEMP).is_err());
    }
    fs::remove_file(lock).unwrap();
    {
        let mounted = fixture.mount(fixture.saves(false).await).await;
        assert_eq!(
            Fixture::bytes(mounted.open(TEMP, false).unwrap()),
            b"original"
        );
        assert!(!mounted.exists(SAVE));
    }
    let mounted = fixture.mount(fixture.saves(false).await).await;
    assert!(!mounted.exists(SAVE));
    assert!(!mounted.exists(TEMP));
}

#[tokio::test]
async fn corrupt_payload_blocks_recovery_until_the_payload_is_repaired() {
    let fixture = Fixture::new().await;
    let lock;
    {
        let mounted = fixture.mount(fixture.saves(true).await).await;
        mounted
            .create(TEMP)
            .unwrap()
            .write_all_at(0, b"recoverable")
            .unwrap();
        lock = fixture.lock_profile();
        assert!(mounted.rename(TEMP, SAVE).is_err());
    }
    fs::remove_file(lock).unwrap();
    let payload = fs::read_dir(fixture.journal_directory())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "payload")
        })
        .unwrap();
    let original = fs::read(&payload).unwrap();
    let saves = fixture.saves(false).await;
    let mut damaged = original.clone();
    damaged[0] ^= 1;
    for bytes in [b"corrupt".as_slice(), damaged.as_slice()] {
        fs::write(&payload, bytes).unwrap();
        assert!(
            fixture
                .wii
                .nand
                .compose(Some(saves.clone()), Arc::new(fixture.wii.keys.clone()))
                .await
                .is_err()
        );
        assert!(fixture.journal_directory().join("pending").exists());
        assert!(saves.checkpoint_saves().await.is_err());
        assert_eq!(
            Fixture::bytes(saves.open(SAVE, false).unwrap()),
            b"recoverable"
        );
    }
    fs::write(payload, original).unwrap();
    let mounted = fixture.mount(saves).await;
    assert_eq!(
        Fixture::bytes(mounted.open(SAVE, false).unwrap()),
        b"recoverable"
    );
}

#[tokio::test]
async fn prepared_record_is_aborted_without_publishing_its_payload() {
    let fixture = Fixture::new().await;
    let saves = fixture.saves(true).await;
    let directory = fixture.journal_directory();
    fs::create_dir_all(&directory).unwrap();
    let transaction = Uuid::new_v4();
    let payload = b"uncommitted";
    fs::write(directory.join(format!("{transaction}.payload")), payload).unwrap();
    let record = serde_json::json!({
        "transaction": transaction, "session": Uuid::new_v4(), "profile": fixture.id,
        "from": { "layer": "scratch", "path": TEMP, "volatile": true },
        "to": { "layer": "saves", "path": SAVE, "volatile": false },
        "committed": false, "length": payload.len(), "digest": blake3::hash(payload).as_bytes(),
    });
    fs::write(
        directory.join("pending"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    assert!(saves.checkpoint_saves().await.is_err());
    let mounted = fixture.mount(saves.clone()).await;
    assert_eq!(
        Fixture::bytes(mounted.open(SAVE, false).unwrap()),
        b"original"
    );
    saves.checkpoint_saves().await.unwrap();
    mounted
        .create(TEMP)
        .unwrap()
        .write_all_at(0, b"next rename")
        .unwrap();
    mounted.rename(TEMP, SAVE).unwrap();
    assert_eq!(
        Fixture::bytes(mounted.open(SAVE, false).unwrap()),
        b"next rename"
    );
}
