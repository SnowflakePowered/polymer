use std::borrow::Cow;

use encoding_rs::{SHIFT_JIS, WINDOWS_1252};

use crate::impls::GameCubeDiscId;
use crate::savegame::SaveDepotId;

pub const DIRENT_SIZE: usize = 0x40;
pub const BLOCK_SIZE: usize = 0x2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Region {
    Japan,
    Usa,
    Pal,
    Unknown(u8),
}

impl Region {
    pub fn from_country_code(code: u8) -> Self {
        match code.to_ascii_uppercase() {
            b'J' | b'K' | b'Q' | b'T' | b'W' => Self::Japan,
            b'B' | b'E' | b'N' => Self::Usa,
            b'D' | b'F' | b'H' | b'I' | b'L' | b'M' | b'P' | b'R' | b'S' | b'U' | b'V' | b'X'
            | b'Y' | b'Z' => Self::Pal,
            other => Self::Unknown(other),
        }
    }

    pub fn is_shift_jis(self) -> bool {
        self == Self::Japan
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("a GCI file is shorter than its 64-byte directory entry")]
    Truncated,
    #[error("the GCI directory entry is unused")]
    Unused,
    #[error("the GCI game and maker code is invalid")]
    InvalidGameCode,
    #[error("the GCI block count {0} is invalid")]
    InvalidBlockCount(u16),
    #[error("the GCI length is {actual} bytes but its directory entry requires {expected}")]
    Length { actual: usize, expected: usize },
}

#[derive(Clone)]
pub struct Save<'a> {
    bytes: Cow<'a, [u8]>,
}

impl<'a> Save<'a> {
    pub fn new(bytes: impl Into<Cow<'a, [u8]>>) -> Result<Self, ParseError> {
        let save = Self {
            bytes: bytes.into(),
        };
        save.validate()?;
        Ok(save)
    }

    fn validate(&self) -> Result<(), ParseError> {
        let dirent = self.bytes.get(..DIRENT_SIZE).ok_or(ParseError::Truncated)?;
        if dirent[..4] == [0xff; 4] {
            return Err(ParseError::Unused);
        }
        GameCubeDiscId::new(&dirent[..6]).ok_or(ParseError::InvalidGameCode)?;
        let blocks = u16::from_be_bytes([dirent[0x38], dirent[0x39]]);
        if blocks == 0 {
            return Err(ParseError::InvalidBlockCount(blocks));
        }
        let expected = DIRENT_SIZE + usize::from(blocks) * BLOCK_SIZE;
        if self.bytes.len() != expected {
            return Err(ParseError::Length {
                actual: self.bytes.len(),
                expected,
            });
        }
        Ok(())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_owned(self) -> Save<'static> {
        Save {
            bytes: Cow::Owned(self.bytes.into_owned()),
        }
    }

    pub fn dirent(&self) -> &[u8; DIRENT_SIZE] {
        self.bytes[..DIRENT_SIZE]
            .try_into()
            .expect("a validated GCI has a directory entry")
    }

    pub fn data(&self) -> &[u8] {
        &self.bytes[DIRENT_SIZE..]
    }

    pub fn block_count(&self) -> u16 {
        u16::from_be_bytes([self.bytes[0x38], self.bytes[0x39]])
    }

    pub fn game_code(&self) -> &[u8; 4] {
        self.bytes[..4].try_into().expect("four bytes")
    }

    pub fn maker_code(&self) -> &[u8; 2] {
        self.bytes[4..6].try_into().expect("two bytes")
    }

    pub fn product_code(&self) -> GameCubeDiscId {
        GameCubeDiscId::new(&self.bytes[..6]).expect("a validated GCI has a product code")
    }

    pub fn depot_id(&self) -> SaveDepotId {
        self.product_code().into()
    }

    pub fn region(&self) -> Region {
        Region::from_country_code(self.game_code()[3])
    }

    pub fn console_name(&self) -> &[u8] {
        let name = &self.bytes[8..0x28];
        &name[..name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(name.len())]
    }

    pub fn identity(&self) -> Vec<u8> {
        let mut identity = Vec::with_capacity(6 + self.console_name().len());
        identity.extend_from_slice(&self.bytes[..6]);
        identity.extend_from_slice(self.console_name());
        identity
    }

    pub fn file_name(&self) -> String {
        let mut name = format!(
            "{}-{}-{}.gci",
            self.decode_text(self.maker_code()),
            self.decode_text(self.game_code()),
            self.decode_text(self.console_name())
        );
        for character in ['<', '>', ':', '"', '/', '\\', '|', '?', '*'] {
            name = name.replace(character, "_");
        }
        name.trim_end_matches(['.', ' ']).to_owned()
    }

    pub fn comments(&self) -> Option<(Cow<'_, str>, Cow<'_, str>)> {
        let address = u32::from_be_bytes(self.bytes[0x3c..0x40].try_into().expect("four bytes"));
        let comments = self.data().get(address as usize..address as usize + 0x40)?;
        Some((
            self.decode_text(&comments[..0x20]),
            self.decode_text(&comments[0x20..]),
        ))
    }

    fn decode_text<'text>(&self, text: &'text [u8]) -> Cow<'text, str> {
        let end = text
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(text.len());
        if self.region().is_shift_jis() {
            SHIFT_JIS.decode_without_bom_handling(&text[..end]).0
        } else {
            WINDOWS_1252.decode_without_bom_handling(&text[..end]).0
        }
    }
}

impl std::fmt::Debug for Save<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GameCubeGci")
            .field("game_code", &String::from_utf8_lossy(self.game_code()))
            .field("maker_code", &String::from_utf8_lossy(self.maker_code()))
            .field(
                "console_name",
                &String::from_utf8_lossy(self.console_name()),
            )
            .field("blocks", &self.block_count())
            .finish()
    }
}

#[cfg(test)]
pub fn fixture(product_code: &[u8; 6], name: &[u8], blocks: u16) -> Save<'static> {
    let mut bytes = vec![0; DIRENT_SIZE + usize::from(blocks) * BLOCK_SIZE];
    bytes[..6].copy_from_slice(product_code);
    bytes[6] = 0xff;
    bytes[8..8 + name.len()].copy_from_slice(name);
    bytes[0x36..0x38].copy_from_slice(&0xffffu16.to_be_bytes());
    bytes[0x38..0x3a].copy_from_slice(&blocks.to_be_bytes());
    bytes[0x3a..0x3c].fill(0xff);
    bytes[0x3c..0x40].fill(0xff);
    Save::new(bytes).unwrap().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_and_preserves_a_gci_without_copying() {
        let bytes = fixture(b"GALE01", b"SmashData", 2).bytes.into_owned();
        let save = Save::new(bytes.as_slice()).unwrap();
        assert!(matches!(save.bytes, Cow::Borrowed(_)));
        assert_eq!(save.block_count(), 2);
        assert_eq!(save.region(), Region::Usa);
        assert_eq!(save.console_name(), b"SmashData");
        assert_eq!(save.as_bytes().as_ptr(), bytes.as_ptr());
    }

    #[test]
    fn rejects_a_length_that_disagrees_with_the_directory_entry() {
        let mut bytes = fixture(b"GZLE01", b"Zelda", 1).bytes.into_owned();
        bytes.pop();
        assert!(matches!(Save::new(bytes), Err(ParseError::Length { .. })));
    }
}
