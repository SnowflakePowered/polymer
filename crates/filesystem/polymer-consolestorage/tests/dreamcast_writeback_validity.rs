use std::path::PathBuf;
use std::sync::Arc;

use polymer_consolestorage::impls::DreamcastProductId;
use polymer_consolestorage::impls::dreamcast::DreamcastMemoryCard;
use polymer_consolestorage::impls::dreamcast::vmi::Vmi;
use polymer_consolestorage::savegame::SaveDepotId;
use polymer_consolestorage::{
    MemoryCard, MemoryCardView, PlatformRepository, Storage, StorageFile, StorageKind,
    VersionedStorage,
};
use uuid::Uuid;

const BLOCK: usize = 512;
const IMAGE_SIZE: usize = 256 * BLOCK;
const ROOT_OFFSET: usize = 255 * BLOCK;
const FAT_OFFSET: usize = 254 * BLOCK;
const DIRECTORY_OFFSET: usize = 253 * BLOCK;
const PATHS: [[&str; 2]; 2] = [
    ["card/T0001/SAVEA.vmi", "card/T0001/SAVEA.vms"],
    ["card/T0001/SAVEB.vmi", "card/T0001/SAVEB.vms"],
];

struct Fixture {
    _directory: tempfile::TempDir,
    checkout: PathBuf,
    _card: DreamcastMemoryCard,
    _view: MemoryCardView,
    file: Arc<dyn StorageFile>,
    image: Vec<u8>,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let platform =
            PlatformRepository::open(directory.path().join("platform"), "SEGA_DC").unwrap();
        let id = Uuid::new_v4();
        let volume = platform.volume(StorageKind::MemoryCard, id);
        let checkout = volume.checkout_path().to_owned();
        let card = DreamcastMemoryCard::new_empty(VersionedStorage::new(volume).unwrap(), id, None)
            .await
            .unwrap();
        for ([vmi_path, vms_path], (name, fill)) in
            PATHS.iter().zip([(b"SAVEA", 0xa1), (b"SAVEB", 0xb2)])
        {
            let mut vms = vec![fill; 2 * BLOCK];
            vms[..0x80].fill(0);
            let mut resource = [0; 8];
            resource[..5].copy_from_slice(name);
            let mut file_name = [b'_'; 12];
            file_name[..5].copy_from_slice(name);
            let vmi = Vmi::synthesize(
                resource,
                file_name,
                b"Writeback",
                b"Polymer",
                [0x20, 0x26, 0x10, 0x09, 0x12, 0, 0, 0x05],
                0,
                vms.len(),
            )
            .unwrap();
            card.install_bytes(&vms, vms_path).unwrap();
            card.install_bytes(vmi.as_bytes(), vmi_path).unwrap();
        }
        let primary: SaveDepotId = DreamcastProductId::new(b"T0001").unwrap().into();
        let view = card.raw_view(&[primary]).unwrap();
        let file = view.storage().open("", true).unwrap();
        let mut image = vec![0; IMAGE_SIZE];
        assert_eq!(file.read_at(0, &mut image).unwrap(), IMAGE_SIZE);
        Self {
            _directory: directory,
            checkout,
            _card: card,
            _view: view,
            file,
            image,
        }
    }

    fn stored(&self) -> Vec<Vec<u8>> {
        PATHS
            .iter()
            .flatten()
            .map(|path| std::fs::read(self.checkout.join(path)).unwrap())
            .collect()
    }

    fn data_block(&self, fill: u8) -> usize {
        self.image
            .chunks_exact(BLOCK)
            .position(|block| block.iter().all(|byte| *byte == fill))
            .unwrap()
            * BLOCK
    }

    fn first_block(&self, name: &[u8]) -> usize {
        let entry = self.image[DIRECTORY_OFFSET..DIRECTORY_OFFSET + BLOCK]
            .chunks_exact(32)
            .find(|entry| entry[4..].starts_with(name))
            .unwrap();
        usize::from(u16::from_le_bytes([entry[2], entry[3]]))
    }

    fn write(&self, offset: usize, bytes: &[u8]) {
        self.file.write_all_at(offset as u64, bytes).unwrap();
    }
}

#[tokio::test]
async fn a_torn_save_is_kept_while_valid_saves_write_back() {
    let fixture = Fixture::new().await;
    let before = fixture.stored();

    fixture.write(fixture.data_block(0xa1), &[0xc3; BLOCK]);
    let broken = FAT_OFFSET + fixture.first_block(b"SAVEB") * 2;
    fixture.write(broken, &0xfffcu16.to_le_bytes());
    fixture.write(fixture.data_block(0xb2), &[0xd4; BLOCK]);
    fixture.file.sync().unwrap();

    let after = fixture.stored();
    assert_eq!(&after[1][BLOCK..], &[0xc3; BLOCK]);
    assert_eq!(after[2..], before[2..]);
}

#[tokio::test]
async fn an_invalid_root_block_stores_nothing() {
    let fixture = Fixture::new().await;
    let before = fixture.stored();

    fixture.write(fixture.data_block(0xa1), &[0xc3; BLOCK]);
    fixture.write(ROOT_OFFSET, &[0; 16]);
    fixture.file.sync().unwrap();

    assert_eq!(fixture.stored(), before);
}
