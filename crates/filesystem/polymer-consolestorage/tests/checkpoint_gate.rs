use std::path::Path;
use std::sync::{Arc, Mutex};

use polymer_consolestorage::impls::ps1::PlaystationMemoryCard;
use polymer_consolestorage::savegame::SaveGameSink;
use polymer_consolestorage::{
    ChangeKind, ChangedEntry, CheckpointGate, CheckpointRequest, PlatformRepository, SaveRepo,
    Storage, StorageKind, VersionedStorage,
};
use uuid::Uuid;

const ACCEPTED: &str = "card/BASLUS-00001";
const REJECTED: &str = "card/BASLUS-00002";

fn save(fill: u8) -> Vec<u8> {
    let mut payload = vec![fill; 8192];
    payload[..4].copy_from_slice(&[b'S', b'C', 0x11, 1]);
    payload
}

fn history_len(card: &PlaystationMemoryCard, root: &str) -> usize {
    let id = card
        .enumerate_saves()
        .find(|save| save.root_path == root)
        .unwrap()
        .id;
    card.save(id).history().unwrap().len()
}

#[tokio::test]
async fn a_rejected_save_stays_pending_while_other_saves_checkpoint() {
    let directory = tempfile::tempdir().unwrap();
    let platform = PlatformRepository::open(directory.path().join("platform"), "SONY_PSX").unwrap();
    let id = Uuid::new_v4();
    let volume = platform.volume(StorageKind::MemoryCard, id);
    let checkout = volume.checkout_path().to_owned();
    let card = PlaystationMemoryCard::new_empty(VersionedStorage::new(volume).unwrap(), id, None)
        .await
        .unwrap();
    card.install_bytes(&save(1), ACCEPTED).unwrap();
    card.install_bytes(&save(2), REJECTED).unwrap();
    card.checkpoint_saves().await.unwrap();

    card.install_bytes(&save(3), ACCEPTED).unwrap();
    card.install_bytes(&save(4), REJECTED).unwrap();
    let requests = Arc::new(Mutex::new(Vec::<CheckpointRequest>::new()));
    let recorded = requests.clone();
    let gate: CheckpointGate = Box::new(move |request: CheckpointRequest| {
        let ready = request.root != REJECTED;
        recorded.lock().unwrap().push(request);
        Box::pin(async move { ready })
    });
    card.checkpoint_saves_gated(Some(&gate)).await.unwrap();

    let mut requests = requests.lock().unwrap().clone();
    requests.sort_by(|left, right| left.root.cmp(&right.root));
    let changes = requests
        .iter()
        .map(|request| (request.root.as_str(), request.changed.clone()))
        .collect::<Vec<_>>();
    let changed = |path: &str| {
        vec![ChangedEntry {
            path: path.to_owned(),
            kind: ChangeKind::File,
        }]
    };
    assert_eq!(
        changes,
        [(ACCEPTED, changed(ACCEPTED)), (REJECTED, changed(REJECTED))]
    );
    assert_eq!(history_len(&card, ACCEPTED), 2);
    assert_eq!(history_len(&card, REJECTED), 1);

    drop(card);
    let reopened = platform.volume(StorageKind::MemoryCard, id);
    assert!(reopened.exists(ACCEPTED));
    assert_eq!(
        std::fs::read(checkout.join(Path::new(ACCEPTED))).unwrap(),
        save(3)
    );
    assert_eq!(
        std::fs::read(checkout.join(Path::new(REJECTED))).unwrap(),
        save(2)
    );
}
