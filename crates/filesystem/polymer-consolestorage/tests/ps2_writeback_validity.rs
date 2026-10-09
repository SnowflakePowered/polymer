use std::path::{Path, PathBuf};

use polymer_consolestorage::impls::ps2::Playstation2MemoryCard;
use polymer_consolestorage::{
    MemoryCard, PlatformRepository, Storage, StorageKind, VersionedStorage,
};
use uuid::Uuid;

const KEPT: &str = "BASLUS-20001";
const UPDATED: &str = "BASLUS-20002";
const DATA: &str = "DATA";

fn dirent(mode: u32, length: u32, name: &str) -> Vec<u8> {
    let mut bytes = vec![0; 512];
    bytes[..4].copy_from_slice(&mode.to_le_bytes());
    bytes[4..8].copy_from_slice(&length.to_le_bytes());
    bytes[8..16].copy_from_slice(&[0, 10, 20, 12, 3, 4, 0xd2, 0x07]);
    bytes[0x18..0x20].copy_from_slice(&[0, 11, 21, 13, 5, 6, 0xd3, 0x07]);
    bytes[0x40..0x40 + name.len()].copy_from_slice(name.as_bytes());
    bytes
}

fn psu(name: &str, data: &[u8]) -> Vec<u8> {
    let mut bytes = dirent(0x8427, 3, name);
    bytes.extend(dirent(0x8427, 0, "."));
    bytes.extend(dirent(0x8427, 0, ".."));
    bytes.extend(dirent(0x8497, data.len() as u32, DATA));
    bytes.extend_from_slice(data);
    bytes.resize(bytes.len().next_multiple_of(1024), 0);
    bytes
}

fn canonical(name: &str) -> String {
    format!("card/{name}.psu")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

async fn card(directory: &Path) -> (Playstation2MemoryCard, PathBuf) {
    let platform = PlatformRepository::open(directory.join("platform"), "SONY_PS2").unwrap();
    let id = Uuid::new_v4();
    let volume = platform.volume(StorageKind::MemoryCard, id);
    let checkout = volume.checkout_path().to_owned();
    let card = Playstation2MemoryCard::new_empty(VersionedStorage::new(volume).unwrap(), id, None)
        .await
        .unwrap();
    card.install_bytes(&psu(KEPT, &[0xa5; 3000]), &canonical(KEPT))
        .unwrap();
    card.install_bytes(&psu(UPDATED, &[0xb6; 3000]), &canonical(UPDATED))
        .unwrap();
    (card, checkout)
}

fn assert_written_back(checkout: &Path) {
    assert_eq!(
        std::fs::read(checkout.join(canonical(KEPT))).unwrap(),
        psu(KEPT, &[0xa5; 3000])
    );
    assert!(contains(
        &std::fs::read(checkout.join(canonical(UPDATED))).unwrap(),
        &[0x5a; 64]
    ));
}

#[tokio::test]
async fn a_pcsx2_save_with_invalid_metadata_is_kept_while_valid_saves_are_written() {
    let directory = tempfile::tempdir().unwrap();
    let (card, checkout) = card(directory.path()).await;
    let view = card.view(&[], "pcsx2").unwrap();
    let storage = view.storage();

    storage
        .open(&format!("{KEPT}/_pcsx2_meta_directory"), true)
        .unwrap()
        .write_all_at(0, &dirent(0x8497, 3, KEPT))
        .unwrap();
    let data = format!("{UPDATED}/{DATA}");
    storage
        .open(&data, true)
        .unwrap()
        .write_all_at(0, &[0x5a; 64])
        .unwrap();
    storage.sync(&data).unwrap();

    assert_written_back(&checkout);
}

#[tokio::test]
async fn a_raw_save_being_created_is_kept_while_valid_saves_are_written() {
    let directory = tempfile::tempdir().unwrap();
    let (card, checkout) = card(directory.path()).await;
    let mut image = card.raw_image(&[]).unwrap();
    let view = card.view(&[], "raw").unwrap();
    let storage = view.storage();

    let name = image
        .windows(KEPT.len())
        .position(|window| window == KEPT.as_bytes())
        .unwrap();
    image[name - 0x40 + 4..name - 0x40 + 8].copy_from_slice(&2u32.to_le_bytes());
    let data = image
        .windows(64)
        .position(|window| window == [0xb6; 64])
        .unwrap();
    image[data..data + 64].fill(0x5a);
    storage
        .open("", true)
        .unwrap()
        .write_all_at(0, &image)
        .unwrap();
    storage.sync("").unwrap();

    assert_written_back(&checkout);
}
