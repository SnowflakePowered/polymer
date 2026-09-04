use std::borrow::Cow;

use zerocopy::byteorder::little_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

pub const SIZE: usize = 108;

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct RawVmi {
    checksum: U32,
    description: [u8; 32],
    copyright: [u8; 32],
    year: U16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    weekday: u8,
    version: U16,
    file_number: U16,
    resource_name: [u8; 8],
    file_name: [u8; 12],
    mode: U16,
    unknown: U16,
    file_size: U32,
}

const _: () = assert!(size_of::<RawVmi>() == SIZE);

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("a VMI file must be exactly 108 bytes")]
    InvalidSize,
    #[error("the VMI layout is invalid")]
    InvalidLayout,
    #[error("the VMI resource or VMU filename is empty")]
    MissingName,
    #[error("the VMI resource checksum is invalid")]
    InvalidChecksum,
    #[error("the VMI file size is too large")]
    FileTooLarge,
}

#[derive(Clone)]
pub struct Vmi<'a> {
    bytes: Cow<'a, [u8]>,
}

impl<'a> Vmi<'a> {
    pub fn new(bytes: impl Into<Cow<'a, [u8]>>) -> Result<Self, ParseError> {
        let vmi = Self {
            bytes: bytes.into(),
        };
        let raw = vmi.raw()?;
        if raw.resource_name.iter().all(|byte| *byte == 0)
            || raw.file_name.iter().all(|byte| *byte == 0)
        {
            return Err(ParseError::MissingName);
        }
        if raw.checksum.get() != Self::checksum(&raw.resource_name) {
            return Err(ParseError::InvalidChecksum);
        }
        Ok(vmi)
    }

    pub fn synthesize(
        resource_name: [u8; 8],
        file_name: [u8; 12],
        description: &[u8],
        copyright: &[u8],
        timestamp: [u8; 8],
        mode: u16,
        file_size: usize,
    ) -> Result<Vmi<'static>, ParseError> {
        let mut bytes = vec![0; SIZE];
        let raw = RawVmi::mut_from_bytes(&mut bytes).map_err(|_| ParseError::InvalidLayout)?;
        let description_len = description.len().min(raw.description.len());
        raw.description[..description_len].copy_from_slice(&description[..description_len]);
        let copyright_len = copyright.len().min(raw.copyright.len());
        raw.copyright[..copyright_len].copy_from_slice(&copyright[..copyright_len]);
        raw.resource_name = resource_name;
        raw.file_name = file_name;
        raw.mode = U16::new(mode);
        raw.file_number = U16::new(1);
        raw.file_size = U32::new(u32::try_from(file_size).map_err(|_| ParseError::FileTooLarge)?);

        let from_bcd = |value: u8| u16::from(value >> 4) * 10 + u16::from(value & 0x0f);
        raw.year = U16::new(from_bcd(timestamp[0]) * 100 + from_bcd(timestamp[1]));
        raw.month = from_bcd(timestamp[2]) as u8;
        raw.day = from_bcd(timestamp[3]) as u8;
        raw.hour = from_bcd(timestamp[4]) as u8;
        raw.minute = from_bcd(timestamp[5]) as u8;
        raw.second = from_bcd(timestamp[6]) as u8;
        raw.weekday = from_bcd(timestamp[7]) as u8;
        raw.checksum = U32::new(Self::checksum(&raw.resource_name));
        Vmi::new(bytes)
    }

    pub fn updated_from(&self, other: &Vmi<'_>) -> Result<Vmi<'static>, ParseError> {
        let mut bytes = self.bytes.to_vec();
        let raw = RawVmi::mut_from_bytes(&mut bytes).map_err(|_| ParseError::InvalidLayout)?;
        let update = other.raw()?;
        raw.year = update.year;
        raw.month = update.month;
        raw.day = update.day;
        raw.hour = update.hour;
        raw.minute = update.minute;
        raw.second = update.second;
        raw.weekday = update.weekday;
        raw.file_name = update.file_name;
        raw.mode = update.mode;
        raw.file_size = update.file_size;
        Vmi::new(bytes)
    }

    fn raw(&self) -> Result<&RawVmi, ParseError> {
        if self.bytes.len() != SIZE {
            return Err(ParseError::InvalidSize);
        }
        RawVmi::ref_from_bytes(&self.bytes).map_err(|_| ParseError::InvalidLayout)
    }

    fn checksum(resource_name: &[u8; 8]) -> u32 {
        u32::from_le_bytes([
            resource_name[0] & b'S',
            resource_name[1] & b'E',
            resource_name[2] & b'G',
            resource_name[3] & b'A',
        ])
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_owned(self) -> Vmi<'static> {
        Vmi {
            bytes: Cow::Owned(self.bytes.into_owned()),
        }
    }

    pub fn description(&self) -> &[u8; 32] {
        &self
            .raw()
            .expect("a validated VMI remains valid")
            .description
    }

    pub fn copyright(&self) -> &[u8; 32] {
        &self.raw().expect("a validated VMI remains valid").copyright
    }

    pub fn resource_name(&self) -> &[u8; 8] {
        &self
            .raw()
            .expect("a validated VMI remains valid")
            .resource_name
    }

    pub fn file_name(&self) -> &[u8; 12] {
        &self.raw().expect("a validated VMI remains valid").file_name
    }

    pub fn mode(&self) -> u16 {
        self.raw()
            .expect("a validated VMI remains valid")
            .mode
            .get()
    }

    pub fn is_game(&self) -> bool {
        self.mode() & 2 != 0
    }

    pub fn is_protected(&self) -> bool {
        self.mode() & 1 != 0
    }

    pub fn file_size(&self) -> usize {
        self.raw()
            .expect("a validated VMI remains valid")
            .file_size
            .get() as usize
    }

    pub fn timestamp(&self) -> [u8; 8] {
        let raw = self.raw().expect("a validated VMI remains valid");
        let to_bcd = |value: u16| (((value / 10) << 4) | (value % 10)) as u8;
        let year = raw.year.get();
        [
            to_bcd(year / 100),
            to_bcd(year % 100),
            to_bcd(u16::from(raw.month)),
            to_bcd(u16::from(raw.day)),
            to_bcd(u16::from(raw.hour)),
            to_bcd(u16::from(raw.minute)),
            to_bcd(u16::from(raw.second)),
            to_bcd(u16::from(raw.weekday)),
        ]
    }

    pub fn resource_stem(&self) -> String {
        let name = &self.resource_name()[..self
            .resource_name()
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(8)];
        name.iter()
            .map(|byte| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' => char::from(*byte),
                _ => '_',
            })
            .collect()
    }
}

impl std::fmt::Debug for Vmi<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Vmi")
            .field("resource", &String::from_utf8_lossy(self.resource_name()))
            .field("file", &String::from_utf8_lossy(self.file_name()))
            .field("size", &self.file_size())
            .finish()
    }
}
