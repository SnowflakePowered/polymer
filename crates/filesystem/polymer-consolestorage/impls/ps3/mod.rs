mod import;
mod nand;
pub mod depots;
mod save_repo;

pub use nand::Ps3Nand;
pub use save_repo::Ps3SaveRepo;

use crate::savegame::SaveDepotId;

const GUEST_SAVE_ROOT: &str = "dev_hdd0/home/00000001/savedata";
const GUEST_USER_ROOT: &str = "dev_hdd0/home/00000001";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TitleId {
    depot: SaveDepotId,
}

impl TitleId {
    fn parse(value: &str) -> Option<Self> {
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

    fn from_directory(name: &str) -> Option<Self> {
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
}
