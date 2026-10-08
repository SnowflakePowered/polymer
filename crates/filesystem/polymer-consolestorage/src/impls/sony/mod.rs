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
