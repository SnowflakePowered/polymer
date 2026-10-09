use std::path::{Path, PathBuf};

use polymer_consolestorage::impls::ps1::PlaystationMemoryCard;
use polymer_consolestorage::{
    PlatformRepository, SaveRepo, Storage, StorageKind, VersionedStorage,
};
use uuid::Uuid;

const KEPT: &str = "card/BASLUS-00001";
const DELETED: &str = "card/BASLUS-00002";
const CREATED: &str = "card/BASLUS-00003";

fn save(fill: u8) -> Vec<u8> {
    let mut payload = vec![fill; 8192];
    payload[..4].copy_from_slice(&[b'S', b'C', 0x11, 1]);
    payload
}

struct Profile {
    platform: PlatformRepository,
    id: Uuid,
    checkout: PathBuf,
    _directory: tempfile::TempDir,
}

impl Profile {
    async fn committed() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let platform =
            PlatformRepository::open(directory.path().join("platform"), "SONY_PSX").unwrap();
        let id = Uuid::new_v4();
        let volume = platform.volume(StorageKind::MemoryCard, id);
        let checkout = volume.checkout_path().to_owned();
        let card =
            PlaystationMemoryCard::new_empty(VersionedStorage::new(volume).unwrap(), id, None)
                .await
                .unwrap();
        card.install_bytes(&save(1), KEPT).unwrap();
        card.install_bytes(&save(2), DELETED).unwrap();
        card.checkpoint_saves().await.unwrap();
        Self {
            platform,
            id,
            checkout,
            _directory: directory,
        }
    }

    fn file(&self, path: &str) -> PathBuf {
        self.checkout.join(Path::new(path))
    }

    fn recovered(&self, path: &str) -> Option<Vec<u8>> {
        let repo = gix::open(self.platform.path()).unwrap();
        let reference = format!(
            "refs/consoles/recovered/v2/memory-card/{}",
            self.id.simple()
        );
        let tree = repo
            .find_reference(reference.as_str())
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .tree()
            .unwrap();
        tree.lookup_entry_by_path(format!("files/{path}"))
            .unwrap()
            .map(|entry| entry.object().unwrap().detach().data)
    }
}

#[tokio::test]
async fn reopening_after_a_crash_preserves_the_checkout_then_restores_the_last_commit() {
    let profile = Profile::committed().await;
    std::fs::write(profile.file(KEPT), save(7)).unwrap();
    std::fs::remove_file(profile.file(DELETED)).unwrap();
    std::fs::write(profile.file(CREATED), save(9)).unwrap();

    let volume = profile.platform.volume(StorageKind::MemoryCard, profile.id);
    assert!(volume.exists(DELETED));

    assert_eq!(std::fs::read(profile.file(KEPT)).unwrap(), save(1));
    assert_eq!(std::fs::read(profile.file(DELETED)).unwrap(), save(2));
    assert!(!profile.file(CREATED).exists());
    assert_eq!(profile.recovered(KEPT), Some(save(7)));
    assert_eq!(profile.recovered(DELETED), None);
    assert_eq!(profile.recovered(CREATED), Some(save(9)));
}

#[tokio::test]
async fn opening_a_volume_during_a_session_keeps_its_uncommitted_changes() {
    let profile = Profile::committed().await;
    let session = profile.platform.volume(StorageKind::MemoryCard, profile.id);
    session.remove(DELETED).unwrap();
    session
        .create(CREATED)
        .unwrap()
        .write_all_at(0, &save(9))
        .unwrap();

    let other = profile.platform.volume(StorageKind::MemoryCard, profile.id);
    assert!(!other.exists(DELETED));

    assert!(!profile.file(DELETED).exists());
    assert_eq!(std::fs::read(profile.file(CREATED)).unwrap(), save(9));
}
