use std::borrow::Cow;

use encoding_rs::SHIFT_JIS;
use zerocopy::byteorder::little_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::vmi::{self, Vmi};

pub const BLOCK_SIZE: usize = 512;
const HEADER_SIZE: usize = 0x80;

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct RawHeader {
    short_description: [u8; 16],
    long_description: [u8; 32],
    application_id: [u8; 16],
    icon_count: U16,
    animation_speed: U16,
    eyecatch_type: U16,
    crc: U16,
    data_size: U32,
    reserved: [u8; 20],
    palette: [U16; 16],
}

const _: () = assert!(size_of::<RawHeader>() == HEADER_SIZE);

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error(transparent)]
    Vmi(#[from] vmi::ParseError),
    #[error("the VMS size does not match its VMI metadata")]
    SizeMismatch,
    #[error("the VMS header is truncated or malformed")]
    InvalidHeader,
    #[error("the VMS header declares an unsupported eyecatch type")]
    InvalidEyecatch,
    #[error("the VMS header graphics exceed the file")]
    TruncatedGraphics,
    #[error("the VMS data payload exceeds the file")]
    TruncatedData,
}

#[derive(Clone)]
pub struct Save<'a> {
    vmi: Vmi<'a>,
    vms: Cow<'a, [u8]>,
}

pub struct Header<'a> {
    raw: &'a RawHeader,
    icons: &'a [u8],
}

impl<'a> Save<'a> {
    pub fn new(
        vmi: impl Into<Cow<'a, [u8]>>,
        vms: impl Into<Cow<'a, [u8]>>,
    ) -> Result<Self, ParseError> {
        let save = Self {
            vmi: Vmi::new(vmi)?,
            vms: vms.into(),
        };
        if save.vmi.file_size() != save.vms.len() {
            return Err(ParseError::SizeMismatch);
        }
        save.header()?;
        Ok(save)
    }

    pub fn from_parts(vmi: Vmi<'a>, vms: impl Into<Cow<'a, [u8]>>) -> Result<Self, ParseError> {
        let save = Self {
            vmi,
            vms: vms.into(),
        };
        if save.vmi.file_size() != save.vms.len() {
            return Err(ParseError::SizeMismatch);
        }
        save.header()?;
        Ok(save)
    }

    pub fn into_owned(self) -> Save<'static> {
        Save {
            vmi: self.vmi.into_owned(),
            vms: Cow::Owned(self.vms.into_owned()),
        }
    }

    pub fn vmi(&self) -> &Vmi<'_> {
        &self.vmi
    }

    pub fn vms(&self) -> &[u8] {
        &self.vms
    }

    pub fn header(&self) -> Result<Header<'_>, ParseError> {
        let offset = usize::from(self.vmi.is_game()) * BLOCK_SIZE;
        let bytes = self.vms.get(offset..).ok_or(ParseError::InvalidHeader)?;
        let raw = RawHeader::ref_from_prefix(bytes)
            .map_err(|_| ParseError::InvalidHeader)?
            .0;
        let icons_size = usize::from(raw.icon_count.get())
            .checked_mul(512)
            .ok_or(ParseError::TruncatedGraphics)?;
        let eyecatch_size = match raw.eyecatch_type.get() {
            0 => 0,
            1 => 8064,
            2 => 4544,
            3 => 2048,
            _ => return Err(ParseError::InvalidEyecatch),
        };
        let graphics = bytes
            .get(HEADER_SIZE..HEADER_SIZE + icons_size + eyecatch_size)
            .ok_or(ParseError::TruncatedGraphics)?;
        let data_end = HEADER_SIZE
            .checked_add(icons_size)
            .and_then(|size| size.checked_add(eyecatch_size))
            .and_then(|size| size.checked_add(raw.data_size.get() as usize))
            .ok_or(ParseError::TruncatedData)?;
        if data_end > bytes.len() {
            return Err(ParseError::TruncatedData);
        }
        Ok(Header {
            raw,
            icons: &graphics[..icons_size],
        })
    }

    pub fn identity(&self) -> [u8; 12] {
        *self.vmi.file_name()
    }

    pub fn block_count(&self) -> usize {
        self.vms.len().div_ceil(BLOCK_SIZE)
    }
}

impl Header<'_> {
    fn text(bytes: &[u8]) -> Cow<'_, str> {
        let end = bytes
            .iter()
            .rposition(|byte| !matches!(*byte, 0 | b' '))
            .map_or(0, |index| index + 1);
        SHIFT_JIS.decode_without_bom_handling(&bytes[..end]).0
    }

    pub fn short_description(&self) -> Cow<'_, str> {
        Self::text(&self.raw.short_description)
    }

    pub fn long_description(&self) -> Cow<'_, str> {
        Self::text(&self.raw.long_description)
    }

    pub fn long_description_bytes(&self) -> &[u8; 32] {
        &self.raw.long_description
    }

    pub fn application_id(&self) -> &[u8; 16] {
        &self.raw.application_id
    }

    pub(crate) fn icons(&self) -> &[u8] {
        self.icons
    }

    pub(crate) fn animation_speed(&self) -> u16 {
        self.raw.animation_speed.get()
    }

    pub(crate) fn icon_color(&self, index: u8) -> u16 {
        self.raw.palette[usize::from(index)].get()
    }
}

impl std::fmt::Debug for Save<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DreamcastSave")
            .field("vmi", &self.vmi)
            .field("blocks", &self.block_count())
            .finish()
    }
}
