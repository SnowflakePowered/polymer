use polymer_consolestorage::impls::ps1::PlaystationMemoryCard;
use polymer_consolestorage::{
    PlatformRepository, SaveRepo, Storage, StorageKind, VersionedStorage,
};
use uuid::Uuid;

const PROFILE: &str = "settings/profile.bin";

#[tokio::test]
async fn profile_writes_through_a_handle_kept_open_across_checkpoints_survive_reopening() {
    let directory = tempfile::tempdir().unwrap();
    let platform = PlatformRepository::open(directory.path().join("platform"), "SONY_PSX").unwrap();
    let id = Uuid::new_v4();
    let card = PlaystationMemoryCard::new_empty(
        VersionedStorage::new(platform.volume(StorageKind::MemoryCard, id)).unwrap(),
        id,
        None,
    )
    .await
    .unwrap();
    card.install_bytes(&[1; 16], PROFILE).unwrap();
    card.checkpoint_saves().await.unwrap();

    let file = card.open(PROFILE, true).unwrap();
    for fill in [2, 3] {
        file.write_at(0, &[fill; 16]).unwrap();
        card.checkpoint_saves().await.unwrap();
    }
    drop(file);
    drop(card);

    let reopened = VersionedStorage::new(platform.volume(StorageKind::MemoryCard, id)).unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut reopened.open_read(PROFILE).unwrap(), &mut bytes).unwrap();
    assert_eq!(bytes, [3; 16]);
}
