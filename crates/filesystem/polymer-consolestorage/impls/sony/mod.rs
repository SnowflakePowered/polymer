pub(crate) mod import;
pub(crate) mod metadata;
pub(crate) mod save_repo;

use crate::savegame::SaveDepotId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TitleId {
    pub(crate) depot: SaveDepotId,
}

impl TitleId {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let bytes = value.as_bytes();
        if bytes.len() != 9
            || !bytes[..4].iter().all(u8::is_ascii_uppercase)
            || !bytes[4..].iter().all(u8::is_ascii_digit)
        {
            return None;
        }
        let letters = bytes[..4]
            .iter()
            .fold(0_u64, |value, byte| value * 26 + u64::from(byte - b'A'));
        let number = value[4..].parse::<u64>().ok()?;
        Some(Self {
            depot: SaveDepotId((letters << 17) | number),
        })
    }

    pub(crate) fn from_directory(name: &str) -> Option<Self> {
        Self::parse(name.get(..9)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_id_families_are_distinct_and_dot_directories_have_no_family() {
        let first = TitleId::parse("AAAA00000").unwrap();
        let last = TitleId::parse("ZZZZ99999").unwrap();
        assert_ne!(first, last);
        assert!(last.depot.0 < (1_u64 << 36));
        let game = TitleId::parse("BLES00001").unwrap();
        assert_eq!(
            TitleId::from_directory("BLES00001-nick01save01"),
            Some(game)
        );
        assert_eq!(TitleId::from_directory(".pending"), None);
        assert_eq!(TitleId::from_directory(".BLES00001-slot"), None);
    }

    #[tokio::test]
    async fn ps3_layout_retains_profile_ownership_and_publication_guard() {
        use crate::impls::ps3::Ps3Nand;
        use crate::savegame::Package;
        use crate::{PlatformRepository, StorageKind, VersionedStorage};
        use uuid::Uuid;

        let directory = tempfile::tempdir().unwrap();
        let platform =
            PlatformRepository::open(directory.path().join("SONY_PS3.git"), Ps3Nand::PLATFORM)
                .unwrap();
        let id = Uuid::now_v7();
        let storage = VersionedStorage::new(platform.volume(StorageKind::SaveRepo, id)).unwrap();
        let saves = Ps3Nand::new_save_repo(storage, id, None).await.unwrap();
        let profile = "dev_hdd0/home/00000001";
        assert!(saves.exists(&format!("{profile}/localusername")));
        assert!(saves.exists(&format!("{profile}/trophy")));
        assert!(saves.claims(&format!("{profile}/trophy/file")));
        assert!(!saves.claims(&format!("{profile}/exdata/license.rap")));
        let depot = saves.infer_save_depot(&Package::new(
            Some("BLUS12345".into()),
            Some("BLES12345".into()),
        ));
        assert_eq!(depot, TitleId::parse("BLES12345").unwrap().depot);
        saves
            .create_dir_all(&format!("{profile}/savedata/BLES12345SLOT1"))
            .unwrap();
        saves
            .create_dir_all(&format!("{profile}/savedata/.pending"))
            .unwrap();
        assert!(matches!(
            saves.export_save(&depot),
            Err(crate::Error::HistoryConflict(_))
        ));
    }
}
