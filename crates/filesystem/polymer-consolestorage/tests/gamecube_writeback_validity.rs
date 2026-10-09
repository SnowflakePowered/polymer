use std::io::Read;

use polymer_consolestorage::impls::gamecube::GameCubeMemoryCard;
use polymer_consolestorage::impls::gamecube::gci::Save;
use polymer_consolestorage::savegame::SaveDepotId;
use polymer_consolestorage::{
    MemoryCard, MountableStorage, PlatformRepository, Storage, StorageKind, VersionedStorage,
};
use uuid::Uuid;

const BLOCK: usize = 0x2000;

struct Fixture {
    _directory: tempfile::TempDir,
    card: GameCubeMemoryCard,
    names: Vec<String>,
    primary: Vec<SaveDepotId>,
}

impl Fixture {
    async fn new(names: &[&str]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let platform = PlatformRepository::open(
            directory.path().join("platform"),
            GameCubeMemoryCard::PLATFORM,
        )
        .unwrap();
        let id = Uuid::new_v4();
        let volume = platform.volume(StorageKind::MemoryCard, id);
        let card = GameCubeMemoryCard::new_empty(VersionedStorage::new(volume).unwrap(), id, None)
            .await
            .unwrap();
        let mut files = Vec::new();
        let mut primary = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let save = Save::new(Self::gci(name, index as u8 + 1)).unwrap();
            let file = save.file_name();
            card.install_bytes(save.as_bytes(), &format!("card/{file}"))
                .unwrap();
            files.push(file);
            primary = vec![save.depot_id()];
        }
        Self {
            _directory: directory,
            card,
            names: files,
            primary,
        }
    }

    fn gci(name: &str, fill: u8) -> Vec<u8> {
        let mut bytes = vec![0; 0x40 + BLOCK];
        bytes[..6].copy_from_slice(b"GALE01");
        bytes[8..8 + name.len()].copy_from_slice(name.as_bytes());
        bytes[0x38..0x3a].copy_from_slice(&1u16.to_be_bytes());
        bytes[0x40..].fill(fill);
        bytes
    }

    fn canonical(&self, index: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        self.card
            .open_read(&format!("card/{}", self.names[index]))
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }
}

fn read_all(storage: &dyn MountableStorage, path: &str) -> Vec<u8> {
    let file = storage.open(path, false).unwrap();
    let mut bytes = vec![0; file.metadata().unwrap().len as usize];
    file.read_at(0, &mut bytes).unwrap();
    bytes
}

fn seal_bat(bat: &mut [u8]) {
    let (mut checksum, mut inverse) = (0u16, 0u16);
    for word in bat[4..].chunks_exact(2) {
        let word = u16::from_be_bytes([word[0], word[1]]);
        checksum = checksum.wrapping_add(word);
        inverse = inverse.wrapping_add(!word);
    }
    let fix = |value: u16| if value == 0xffff { 0 } else { value };
    bat[..2].copy_from_slice(&fix(checksum).to_be_bytes());
    bat[2..4].copy_from_slice(&fix(inverse).to_be_bytes());
}

#[tokio::test]
async fn dolphin_writeback_keeps_truncated_saves_and_stores_valid_ones() {
    let fixture = Fixture::new(&["empty", "half", "edited"]).await;
    let originals = (0..3)
        .map(|index| fixture.canonical(index))
        .collect::<Vec<_>>();
    let view = fixture.card.view(&fixture.primary, "dolphin").unwrap();
    let storage = view.storage();

    storage.truncate(&fixture.names[0], 0).unwrap();
    storage.truncate(&fixture.names[1], 100).unwrap();
    storage
        .open(&fixture.names[2], true)
        .unwrap()
        .write_all_at(0x40, &[0xab; 16])
        .unwrap();
    storage.sync(&fixture.names[2]).unwrap();

    assert_eq!(fixture.canonical(0), originals[0]);
    assert_eq!(fixture.canonical(1), originals[1]);
    let mut expected = originals[2].clone();
    expected[0x40..0x50].fill(0xab);
    assert_eq!(fixture.canonical(2), expected);
}

#[tokio::test]
async fn raw_writeback_keeps_a_broken_chain_and_stores_valid_saves() {
    let fixture = Fixture::new(&["broken", "edited"]).await;
    let originals = (0..2)
        .map(|index| fixture.canonical(index))
        .collect::<Vec<_>>();
    let view = fixture.card.view(&fixture.primary, "raw").unwrap();
    let storage = view.storage();
    let mut image = read_all(storage.as_ref(), "");

    let entries = image[BLOCK..2 * BLOCK].chunks_exact(0x40).take(127);
    let first_block = |name: &[u8]| {
        entries
            .clone()
            .find(|entry| entry[8..8 + name.len()] == *name && entry[8 + name.len()] == 0)
            .map(|entry| usize::from(u16::from_be_bytes([entry[0x36], entry[0x37]])))
            .unwrap()
    };
    let broken = first_block(b"broken");
    let edited = first_block(b"edited");
    let mut bat = image[3 * BLOCK..4 * BLOCK].to_vec();
    let map = 0x0a + (broken - 5) * 2;
    bat[map..map + 2].copy_from_slice(&(broken as u16).to_be_bytes());
    seal_bat(&mut bat);
    image[3 * BLOCK..4 * BLOCK].copy_from_slice(&bat);
    image[4 * BLOCK..5 * BLOCK].copy_from_slice(&bat);
    image[edited * BLOCK..edited * BLOCK + 16].fill(0xcd);

    storage
        .open("", true)
        .unwrap()
        .write_all_at(0, &image)
        .unwrap();
    storage.sync("").unwrap();

    assert_eq!(fixture.canonical(0), originals[0]);
    let mut expected = originals[1].clone();
    expected[0x36..0x38].copy_from_slice(&(edited as u16).to_be_bytes());
    expected[0x40..0x50].fill(0xcd);
    assert_eq!(fixture.canonical(1), expected);
}

#[tokio::test]
async fn raw_writeback_ignores_a_card_with_both_directories_invalid() {
    let fixture = Fixture::new(&["first", "second"]).await;
    let originals = (0..2)
        .map(|index| fixture.canonical(index))
        .collect::<Vec<_>>();
    let view = fixture.card.view(&fixture.primary, "raw").unwrap();
    let storage = view.storage();
    let mut image = read_all(storage.as_ref(), "");

    image[BLOCK..3 * BLOCK].fill(0);
    storage
        .open("", true)
        .unwrap()
        .write_all_at(0, &image)
        .unwrap();
    storage.sync("").unwrap();

    assert_eq!(fixture.canonical(0), originals[0]);
    assert_eq!(fixture.canonical(1), originals[1]);
}

#[tokio::test]
async fn dolphin_writeback_keeps_a_new_save_sharing_a_deleted_saves_path() {
    let fixture = Fixture::new(&["a:b"]).await;
    let original = fixture.canonical(0);
    let view = fixture.card.view(&fixture.primary, "dolphin").unwrap();
    let storage = view.storage();

    storage
        .open(&fixture.names[0], true)
        .unwrap()
        .write_all_at(8, b"a_b")
        .unwrap();
    storage.sync(&fixture.names[0]).unwrap();

    let mut expected = original;
    expected[8..11].copy_from_slice(b"a_b");
    assert_eq!(fixture.canonical(0), expected);
}
