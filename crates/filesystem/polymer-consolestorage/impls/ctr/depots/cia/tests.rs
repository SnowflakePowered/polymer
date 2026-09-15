use super::*;
use crate::installable::IconHint;
use crate::{PlatformRepository, StorageKind};
use sha2::{Digest, Sha256};
use uuid::Uuid;

struct Fixture {
    directory: tempfile::TempDir,
    platform: PlatformRepository,
    nand: CtrNand,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let platform =
            PlatformRepository::open(directory.path().join("nand.git"), CtrNand::PLATFORM).unwrap();
        let id = Uuid::now_v7();
        let nand = CtrNand::new_empty(platform.volume(StorageKind::Nand, id), id, None)
            .await
            .unwrap();
        Self {
            directory,
            platform,
            nand,
        }
    }

    fn ncch(name: &str) -> Vec<u8> {
        let mut bytes = vec![0; 0x3c00];
        bytes[0x100..0x104].copy_from_slice(b"NCCH");
        bytes[0x104..0x108].copy_from_slice(&0x1e_u32.to_le_bytes());
        bytes[0x18f] = 4;
        bytes[0x1a0..0x1a4].copy_from_slice(&1_u32.to_le_bytes());
        bytes[0x1a4..0x1a8].copy_from_slice(&0x1d_u32.to_le_bytes());
        bytes[0x200..0x208].copy_from_slice(b"icon\0\0\0\0");
        bytes[0x20c..0x210].copy_from_slice(&0x36c0_u32.to_le_bytes());
        bytes[0x400..0x404].copy_from_slice(b"SMDH");
        for (index, unit) in name.encode_utf16().enumerate() {
            bytes[0x608 + index * 2..0x60a + index * 2].copy_from_slice(&unit.to_le_bytes());
        }
        bytes
    }

    fn cia(&self, title: u64, ticket_id: u64, contents: &[Vec<u8>], present: &[usize]) -> CiaDepot {
        let mut ticket = vec![0; 0x140 + 0x164 + 0x14];
        ticket[..4].copy_from_slice(&0x10004_u32.to_be_bytes());
        ticket[0x140..0x144].copy_from_slice(b"Root");
        ticket[0x140 + 0x7c] = 1;
        ticket[0x1d0..0x1d8].copy_from_slice(&ticket_id.to_be_bytes());
        ticket[0x1dc..0x1e4].copy_from_slice(&title.to_be_bytes());
        ticket[0x2a8..0x2ac].copy_from_slice(&0x14_u32.to_be_bytes());
        let mut tmd = vec![0; 0x140 + 0xc4 + 0x900 + 0x30 * contents.len()];
        tmd[..4].copy_from_slice(&0x10004_u32.to_be_bytes());
        tmd[0x140..0x144].copy_from_slice(b"Root");
        tmd[0x180] = 1;
        tmd[0x18c..0x194].copy_from_slice(&title.to_be_bytes());
        tmd[0x1dc..0x1de].copy_from_slice(&7_u16.to_be_bytes());
        tmd[0x1de..0x1e0].copy_from_slice(&(contents.len() as u16).to_be_bytes());
        for (index, content) in contents.iter().enumerate() {
            let start = 0xb04 + 0x30 * index;
            tmd[start..start + 4].copy_from_slice(&(index as u32 + 1).to_be_bytes());
            tmd[start + 4..start + 6].copy_from_slice(&(index as u16).to_be_bytes());
            tmd[start + 6..start + 8]
                .copy_from_slice(&(if index == 0 { 0_u16 } else { 0x4000 }).to_be_bytes());
            tmd[start + 8..start + 16].copy_from_slice(&(content.len() as u64).to_be_bytes());
            tmd[start + 16..start + 48].copy_from_slice(&Sha256::digest(content));
        }
        let records_hash = Sha256::digest(&tmd[0xb04..]);
        tmd[0x206..0x208].copy_from_slice(&(contents.len() as u16).to_be_bytes());
        tmd[0x208..0x228].copy_from_slice(&records_hash);
        let info_hash = Sha256::digest(&tmd[0x204..0xb04]);
        tmd[0x1e4..0x204].copy_from_slice(&info_hash);
        let mut bytes = vec![0; 0x2020];
        bytes[..4].copy_from_slice(&0x2020_u32.to_le_bytes());
        bytes[0xc..0x10].copy_from_slice(&(ticket.len() as u32).to_le_bytes());
        bytes[0x10..0x14].copy_from_slice(&(tmd.len() as u32).to_le_bytes());
        let length: u64 = present
            .iter()
            .map(|index| contents[*index].len() as u64)
            .sum();
        bytes[0x18..0x20].copy_from_slice(&length.to_le_bytes());
        for index in present {
            bytes[0x20 + index / 8] |= 0x80 >> (index % 8);
        }
        for section in [ticket, tmd] {
            bytes.resize((bytes.len() + 63) & !63, 0);
            bytes.extend(section);
        }
        bytes.resize((bytes.len() + 63) & !63, 0);
        for index in present {
            bytes.extend(&contents[*index]);
        }
        let path = self.directory.path().join("title.cia");
        std::fs::write(&path, bytes).unwrap();
        CiaDepot::open(&path).unwrap()
    }
}

#[tokio::test]
async fn contents_and_tickets_share_identity_metadata_and_uninstall_in_either_order() {
    for ticket_first in [true, false] {
        let fixture = Fixture::new().await;
        let depot = fixture.cia(
            0x0004_0000_0012_3400,
            1,
            &[Fixture::ncch("Installed title")],
            &[0],
        );
        let ticket = depot.ticket_depot();
        let first = if ticket_first {
            ticket.clone()
        } else {
            depot.clone()
        };
        first.install(&fixture.nand).finish().await.unwrap();
        let second = if ticket_first { depot.clone() } else { ticket };
        let artifact = second.install(&fixture.nand).finish().await.unwrap();
        assert_eq!(artifact.id(), "0004000000123400");
        assert_eq!(artifact.label(), "Installed title");
        assert_eq!(artifact.depot_type(), DepotType::App);
        assert_eq!(artifact.version(), Some("7"));
        assert_eq!(artifact.manifest().len(), 3);
        assert!(matches!(artifact.icon(), IconHint::EmbeddedPng(_)));
        assert_eq!(
            artifact.byte_length(),
            artifact
                .manifest()
                .iter()
                .map(|file| file.byte_length())
                .sum::<u64>()
        );
        let reopened = CtrNand::open(
            fixture
                .platform
                .volume(StorageKind::Nand, fixture.nand.id().await.unwrap()),
        )
        .await
        .unwrap();
        let saved = reopened
            .root()
            .artifacts()
            .artifact(artifact.id())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(saved.icon(), IconHint::EmbeddedPng(bytes) if image::load_from_memory(bytes).is_ok())
        );
        assert_eq!(reopened.artifacts().await.unwrap().len(), 1);
        assert!(!reopened.root().exists("meta/artifacts"));
        reopened.uninstall(&[artifact.id()]).await.unwrap();
        assert!(reopened.artifacts().await.unwrap().is_empty());
        for file in artifact.manifest() {
            assert!(!reopened.root().exists(&file.path().to_string_lossy()));
        }
    }
}

#[tokio::test]
async fn partial_cia_retains_only_matching_omitted_contents_and_preserves_tickets() {
    let fixture = Fixture::new().await;
    let title = 0x0004_008c_0012_3400;
    let contents = vec![b"main".to_vec(), b"optional".to_vec()];
    let depot = fixture.cia(title, 1, &contents, &[0, 1]);
    depot
        .ticket_depot()
        .install(&fixture.nand)
        .finish()
        .await
        .unwrap();
    depot.install(&fixture.nand).finish().await.unwrap();
    let partial = fixture.cia(title, 1, &contents, &[0]);
    let omitted = partial.content_path(2);
    let artifact = partial.install(&fixture.nand).finish().await.unwrap();
    assert!(
        artifact
            .manifest()
            .iter()
            .any(|file| file.path() == Path::new(&omitted))
    );
    let changed = vec![b"main".to_vec(), b"new optional".to_vec()];
    let partial = fixture.cia(title, 2, &changed, &[0]);
    partial
        .ticket_depot()
        .install(&fixture.nand)
        .finish()
        .await
        .unwrap();
    let artifact = partial.install(&fixture.nand).finish().await.unwrap();
    assert!(!fixture.nand.root().exists(&omitted));
    assert_eq!(
        artifact
            .manifest()
            .iter()
            .filter(|file| file.path().to_string_lossy().ends_with(".tik"))
            .count(),
        2
    );
    assert_eq!(artifact.label(), format!("{title:016X}"));
    assert!(matches!(artifact.icon(), IconHint::Builtin(_)));
}

#[tokio::test]
async fn native_checks_and_failed_extraction_never_publish_an_artifact() {
    use futures::StreamExt;
    let fixture = Fixture::new().await;
    let depot = fixture.cia(0x0004_0000_0012_3400, 1, &[Fixture::ncch("Title")], &[0]);
    let mut installation = depot.clone().install(&fixture.nand);
    fixture
        .nand
        .root()
        .create("interrupted")
        .unwrap()
        .write_all_at(0, b"unfinished")
        .unwrap();
    assert!(fixture.nand.try_install(&depot.path).is_err());
    let error = installation.next().await.unwrap().unwrap_err();
    assert!(error.to_string().contains("uncommitted"));
    assert_eq!(
        installation.finish().await.err().unwrap().to_string(),
        error.to_string()
    );
    assert!(!fixture.nand.root().exists(&depot.content_path(1)));
    fixture.nand.recover().unwrap();
    let mut bytes = std::fs::read(&depot.path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(&depot.path, bytes).unwrap();
    assert!(depot.install(&fixture.nand).finish().await.is_err());
    assert!(fixture.nand.artifacts().await.unwrap().is_empty());
    assert!(fixture.nand.is_dirty().unwrap());
}
