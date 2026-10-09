use polymer_consolestorage::impls::ps1::PlaystationMemoryCard;
use polymer_consolestorage::{
    MemoryCard, PlatformRepository, Storage, StorageKind, VersionedStorage,
};
use uuid::Uuid;

const FIRST: &str = "card/BASLUS-00001";
const SECOND: &str = "card/BASLUS-00002";
const BLOCK_SIZE: usize = 8192;
const FRAME_SIZE: usize = 128;

fn save(fill: u8) -> Vec<u8> {
    let mut payload = vec![fill; BLOCK_SIZE];
    payload[..4].copy_from_slice(&[b'S', b'C', 0x11, 1]);
    payload
}

fn frame_checksum(frame: &[u8]) -> u8 {
    frame[..FRAME_SIZE - 1]
        .iter()
        .fold(0, |sum, byte| sum ^ byte)
}

struct Fixture {
    card: PlaystationMemoryCard,
    _directory: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let platform =
            PlatformRepository::open(directory.path().join("platform"), "SONY_PSX").unwrap();
        let id = Uuid::new_v4();
        let card = PlaystationMemoryCard::new_empty(
            VersionedStorage::new(platform.volume(StorageKind::MemoryCard, id)).unwrap(),
            id,
            None,
        )
        .await
        .unwrap();
        card.install_bytes(&save(1), FIRST).unwrap();
        card.install_bytes(&save(2), SECOND).unwrap();
        Self {
            card,
            _directory: directory,
        }
    }

    fn write_back(&self, edit: impl FnOnce(&mut Vec<u8>)) {
        let view = self.card.raw_view(&[]).unwrap();
        let file = view.storage().open("", true).unwrap();
        let mut image = self.card.raw_image(&[]).unwrap();
        edit(&mut image);
        file.write_all_at(0, &image).unwrap();
        file.sync().unwrap();
        view.storage().finish("").unwrap();
    }

    fn canonical(&self, path: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut self.card.open_read(path).unwrap(), &mut bytes).unwrap();
        bytes
    }
}

fn overwrite_data(image: &mut [u8], block: usize, fill: u8) {
    let start = (block + 1) * BLOCK_SIZE + 1024;
    image[start..start + 32].fill(fill);
}

#[tokio::test]
async fn a_torn_save_is_kept_while_other_saves_write_back() {
    let fixture = Fixture::new().await;
    fixture.write_back(|image| {
        let frame = &mut image[FRAME_SIZE..2 * FRAME_SIZE];
        frame[4..8].copy_from_slice(&(2 * BLOCK_SIZE as u32).to_le_bytes());
        frame[FRAME_SIZE - 1] = frame_checksum(frame);
        overwrite_data(image, 0, 91);
        overwrite_data(image, 1, 92);
    });
    assert_eq!(fixture.canonical(FIRST), save(1));
    let mut expected = save(2);
    expected[1024..1024 + 32].fill(92);
    assert_eq!(fixture.canonical(SECOND), expected);
}

#[tokio::test]
async fn an_invalid_card_header_changes_nothing() {
    let fixture = Fixture::new().await;
    fixture.write_back(|image| {
        image[FRAME_SIZE - 1] ^= 0xff;
        overwrite_data(image, 0, 91);
        overwrite_data(image, 1, 92);
    });
    assert_eq!(fixture.canonical(FIRST), save(1));
    assert_eq!(fixture.canonical(SECOND), save(2));
}

#[tokio::test]
async fn a_save_renamed_onto_a_projected_saves_path_is_written_back() {
    let fixture = Fixture::new().await;
    let lossy = "card/BASLUS-00001\u{fffd}";
    fixture.card.install_bytes(&save(3), lossy).unwrap();
    fixture.write_back(|image| {
        let frame = &mut image[2 * FRAME_SIZE..3 * FRAME_SIZE];
        frame[10..31].fill(0);
        frame[10..23].copy_from_slice(b"BASLUS-00001\xff");
        frame[FRAME_SIZE - 1] = frame_checksum(frame);
    });
    assert_eq!(fixture.canonical(lossy), save(3));
}

#[tokio::test]
async fn saves_sharing_a_path_leave_the_stored_save_unchanged() {
    let fixture = Fixture::new().await;
    let lossy = "card/BASLUS-00001\u{fffd}";
    fixture.card.install_bytes(&save(3), lossy).unwrap();
    fixture.write_back(|image| {
        let frame = &mut image[3 * FRAME_SIZE..4 * FRAME_SIZE];
        frame[10..31].fill(0);
        frame[10..23].copy_from_slice(b"BASLUS-00001\xfe");
        frame[FRAME_SIZE - 1] = frame_checksum(frame);
    });
    assert_eq!(fixture.canonical(lossy), save(3));
}

#[tokio::test]
async fn a_save_created_and_deleted_in_one_session_is_not_stored() {
    let fixture = Fixture::new().await;
    let view = fixture.card.raw_view(&[]).unwrap();
    let file = view.storage().open("", true).unwrap();
    let original = fixture.card.raw_image(&[]).unwrap();
    let mut renamed = original.clone();
    let frame = &mut renamed[2 * FRAME_SIZE..3 * FRAME_SIZE];
    frame[10..22].copy_from_slice(b"BASLUS-00003");
    frame[FRAME_SIZE - 1] = frame_checksum(frame);
    for image in [&renamed, &original] {
        file.write_all_at(0, image).unwrap();
        file.sync().unwrap();
    }
    view.storage().finish("").unwrap();
    assert_eq!(fixture.canonical(SECOND), save(2));
    assert!(fixture.card.open_read("card/BASLUS-00003").is_err());
}
