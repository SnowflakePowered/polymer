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
        Self::dirent_console_name(self.dirent())
    }

    fn dirent_console_name(dirent: &[u8; DIRENT_SIZE]) -> &[u8] {
        let name = &dirent[8..0x28];
        &name[..name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(name.len())]
    }

    pub fn dirent_identity(dirent: &[u8; DIRENT_SIZE]) -> Vec<u8> {
        let name = Self::dirent_console_name(dirent);
        let mut identity = Vec::with_capacity(6 + name.len());
        identity.extend_from_slice(&dirent[..6]);
        identity.extend_from_slice(name);
        identity
    }

    pub fn identity(&self) -> Vec<u8> {
        Self::dirent_identity(self.dirent())
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
