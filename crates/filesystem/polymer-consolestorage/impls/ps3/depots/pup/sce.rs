use aes::cipher::{BlockDecryptMut, KeyIvInit, StreamCipher, block_padding::NoPadding};
use aes::{Aes128, Aes256};
use std::io::{self, Read};
use zerocopy::byteorder::big_endian::{U16, U32, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout};

#[derive(FromBytes, Immutable, KnownLayout)]
#[repr(C)]
struct Header {
    magic: [u8; 4],
    version: U32,
    flags: U16,
    kind: U16,
    metadata: U32,
    header_size: U64,
    data_size: U64,
}
#[derive(FromBytes, Immutable, KnownLayout)]
#[repr(C)]
struct Metadata {
    signature_size: U64,
    unknown: U32,
    sections: U32,
    keys: U32,
    optional_size: U32,
    reserved: [u8; 8],
}
#[derive(FromBytes, Immutable, KnownLayout)]
#[repr(C)]
struct Section {
    offset: U64,
    size: U64,
    kind: U32,
    index: U32,
    hashed: U32,
    hash: U32,
    encrypted: U32,
    key: U32,
    iv: U32,
    compressed: U32,
}

pub(super) struct Sce;
impl Sce {
    // Public PS3 firmware package key/IV (PSDevWiki / RPCS3 key_vault.h).
    pub(super) const KEY: [u8; 32] = [
        0xA9, 0x78, 0x18, 0xBD, 0x19, 0x3A, 0x67, 0xA1, 0x6F, 0xE8, 0x3A, 0x85, 0x5E, 0x1B, 0xE9,
        0xFB, 0x56, 0x40, 0x93, 0x8D, 0x4D, 0xBC, 0xB2, 0xCB, 0x52, 0xC5, 0xA2, 0xF8, 0xB0, 0x2B,
        0x10, 0x31,
    ];
    pub(super) const IV: [u8; 16] = [
        0x4A, 0xCE, 0xF0, 0x12, 0x24, 0xFB, 0xEE, 0xDF, 0x82, 0x45, 0xF8, 0xFF, 0x10, 0x21, 0x1E,
        0x6E,
    ];
    pub const MAX_SIZE: u64 = 256 * 1024 * 1024;

    fn structure<T: FromBytes>(data: &[u8], offset: usize) -> io::Result<T> {
        T::read_from_prefix(
            data.get(offset..)
                .ok_or_else(|| io::Error::other("SCE offset outside package"))?,
        )
        .map(|(value, _)| value)
        .map_err(|_| io::Error::other("truncated SCE structure"))
    }
    pub fn flash_tar(bytes: &[u8]) -> io::Result<Vec<u8>> {
        let header: Header = Self::structure(bytes, 0)?;
        if header.magic != *b"SCE\0" || header.kind.get() != 3 {
            return Err(io::Error::other("not an SCE firmware package"));
        }
        let start = 32usize + header.metadata.get() as usize;
        let end = usize::try_from(header.header_size.get()).map_err(io::Error::other)?;
        let info_end = start
            .checked_add(64)
            .ok_or_else(|| io::Error::other("invalid SCE metadata"))?;
        let mut info = bytes
            .get(start..info_end)
            .ok_or_else(|| io::Error::other("truncated SCE key"))?
            .to_vec();
        if header.flags.get() & 0x8000 == 0 {
            cbc::Decryptor::<Aes256>::new(&Self::KEY.into(), &Self::IV.into())
                .decrypt_padded_mut::<NoPadding>(&mut info)
                .map_err(|_| io::Error::other("invalid SCE key"))?;
        }
        if info[16..32] != [0; 16] || info[48..64] != [0; 16] {
            return Err(io::Error::other("SCE metadata key padding mismatch"));
        }
        let mut metadata = bytes
            .get(info_end..end)
            .ok_or_else(|| io::Error::other("invalid SCE metadata range"))?
            .to_vec();
        ctr::Ctr128BE::<Aes128>::new_from_slices(&info[..16], &info[32..48])
            .unwrap()
            .apply_keystream(&mut metadata);
        Self::section(bytes, &metadata)
    }
    fn section(bytes: &[u8], metadata: &[u8]) -> io::Result<Vec<u8>> {
        let header: Metadata = Self::structure(metadata, 0)?;
        let count = header.sections.get() as usize;
        if count < 3 || count > metadata.len().saturating_sub(32) / 48 {
            return Err(io::Error::other("invalid SCE section table"));
        }
        let keys_start = 32 + count * 48;
        let keys_size = (header.keys.get() as usize)
            .checked_mul(16)
            .ok_or_else(|| io::Error::other("invalid SCE key table"))?;
        let keys = metadata
            .get(
                keys_start
                    ..keys_start
                        .checked_add(keys_size)
                        .ok_or_else(|| io::Error::other("invalid SCE key table"))?,
            )
            .ok_or_else(|| io::Error::other("truncated SCE key table"))?;
        // Firmware packages contain two control sections followed by the flash TAR.
        let section: Section = Self::structure(metadata, 32 + 2 * 48)?;
        let offset = usize::try_from(section.offset.get()).map_err(io::Error::other)?;
        let size = usize::try_from(section.size.get()).map_err(io::Error::other)?;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| io::Error::other("invalid SCE data range"))?;
        let mut data = bytes
            .get(offset..end)
            .ok_or_else(|| io::Error::other("truncated SCE data"))?
            .to_vec();
        match section.encrypted.get() {
            1 => {}
            3 => {
                let key = keys
                    .chunks_exact(16)
                    .nth(section.key.get() as usize)
                    .ok_or_else(|| io::Error::other("invalid SCE key index"))?;
                let iv = keys
                    .chunks_exact(16)
                    .nth(section.iv.get() as usize)
                    .ok_or_else(|| io::Error::other("invalid SCE IV index"))?;
                ctr::Ctr128BE::<Aes128>::new_from_slices(key, iv)
                    .unwrap()
                    .apply_keystream(&mut data);
            }
            _ => return Err(io::Error::other("unsupported SCE encryption")),
        }
        match section.compressed.get() {
            1 => Ok(data),
            2 => {
                let mut result = Vec::new();
                flate2::read::ZlibDecoder::new(data.as_slice())
                    .take(Self::MAX_SIZE + 1)
                    .read_to_end(&mut result)?;
                if result.len() as u64 > Self::MAX_SIZE {
                    return Err(io::Error::other("flash TAR exceeds size limit"));
                }
                Ok(result)
            }
            _ => Err(io::Error::other("unsupported SCE compression")),
        }
    }
}
