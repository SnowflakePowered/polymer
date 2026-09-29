use aes::Aes128;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use zerocopy::byteorder::big_endian::{U16, U32, U64};
use zerocopy::{FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::Error;

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(crate) struct TicketBody {
    pub(crate) issuer: [u8; 0x40],
    ecc_public_key: [u8; 0x3c],
    pub(crate) version: u8,
    crl_versions: [u8; 2],
    pub(crate) encrypted_title_key: [u8; 16],
    reserved0: u8,
    pub(crate) ticket_id: U64,
    pub(crate) device_id: U32,
    pub(crate) title_id: U64,
    reserved1: [u8; 2],
    pub(crate) title_version: U16,
    reserved2: [u8; 9],
    pub(crate) common_key_index: u8,
    reserved3: [u8; 0x30],
    pub(crate) content_access: [u8; 0x40],
    reserved4: [u8; 0x42],
}

const _: () = assert!(size_of::<TicketBody>() == 0x164);

impl TicketBody {
    pub(crate) fn issued(issuer: &[u8], title_id: u64, encrypted_title_key: [u8; 16]) -> Self {
        let mut body = Self::new_zeroed();
        body.issuer[..issuer.len()].copy_from_slice(issuer);
        body.ticket_id.set(title_id);
        body.title_id.set(title_id);
        body.encrypted_title_key = encrypted_title_key;
        body
    }

    pub(crate) fn issuer_name(&self) -> &[u8] {
        self.issuer
            .split(|byte| *byte == 0)
            .next()
            .unwrap_or_default()
    }
}

pub(crate) struct TitleKey(pub(crate) [u8; 16]);

impl TitleKey {
    pub(crate) fn new(key: &[u8], file: &str) -> Result<Self, Error> {
        key.try_into()
            .map(Self)
            .map_err(|_| Error::malformed(file, "expected 16 bytes"))
    }

    pub(crate) fn decrypt(common: &[u8; 16], title_id: u64, encrypted: &[u8; 16]) -> Self {
        let mut block = GenericArray::clone_from_slice(encrypted);
        Aes128::new(GenericArray::from_slice(common)).decrypt_block(&mut block);
        let mut key = Self(block.into());
        key.chain(title_id);
        key
    }

    pub(crate) fn encrypt(mut self, common: &[u8; 16], title_id: u64) -> [u8; 16] {
        self.chain(title_id);
        let mut block = GenericArray::from(self.0);
        Aes128::new(GenericArray::from_slice(common)).encrypt_block(&mut block);
        block.into()
    }

    fn chain(&mut self, title_id: u64) {
        for (byte, iv) in self.0[..8].iter_mut().zip(title_id.to_be_bytes()) {
            *byte ^= iv;
        }
    }
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(crate) struct RightsHeader {
    version: U16,
    header_size: U16,
    pub(crate) size: U32,
    offset: U32,
    count: U16,
    stride: U16,
    reserved: U32,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct RightsSection {
    offset: U32,
    count: U32,
    stride: U32,
    size: U32,
    kind: U16,
    reserved: U16,
}

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct RightsRecord {
    reserved: U16,
    base: U16,
    bits: [u8; 0x80],
}

impl RightsHeader {
    const HEADER_SIZE: u16 = size_of::<Self>() as u16;
    const SECTION_SIZE: u16 = size_of::<RightsSection>() as u16;
    const RECORD_SIZE: u32 = size_of::<RightsRecord>() as u32;
    const CONTENT_INDEX: u16 = 3;

    pub(crate) fn synthesize(records: u32) -> Vec<u8> {
        let table = u32::from(Self::HEADER_SIZE + Self::SECTION_SIZE);
        let header = Self {
            version: 1.into(),
            header_size: Self::HEADER_SIZE.into(),
            size: (table + records * Self::RECORD_SIZE).into(),
            offset: u32::from(Self::HEADER_SIZE).into(),
            count: 1.into(),
            stride: Self::SECTION_SIZE.into(),
            reserved: 0.into(),
        };
        let section = RightsSection {
            offset: table.into(),
            count: records.into(),
            stride: Self::RECORD_SIZE.into(),
            size: (records * Self::RECORD_SIZE).into(),
            kind: Self::CONTENT_INDEX.into(),
            reserved: 0.into(),
        };
        let mut bytes = [header.as_bytes(), section.as_bytes()].concat();
        for index in 0..records {
            let record = RightsRecord {
                reserved: 0.into(),
                base: ((index * 1024) as u16).into(),
                bits: [0xff; 0x80],
            };
            bytes.extend_from_slice(record.as_bytes());
        }
        bytes
    }

    pub(crate) fn validate(bytes: &[u8], file: &str) -> Result<(), Error> {
        let error = || Error::malformed(file, "invalid or truncated content rights");
        let (header, _) = Self::ref_from_prefix(bytes).map_err(|_| error())?;
        if header.version.get() != 1
            || header.header_size.get() != Self::HEADER_SIZE
            || header.stride.get() != Self::SECTION_SIZE
        {
            return Err(error());
        }
        let table_start = u64::from(header.offset.get());
        let table_end = table_start + u64::from(header.count.get()) * u64::from(Self::SECTION_SIZE);
        let table = bytes
            .get(table_start as usize..usize::try_from(table_end).map_err(|_| error())?)
            .ok_or_else(error)?;
        for raw in table.chunks_exact(usize::from(Self::SECTION_SIZE)) {
            let section = RightsSection::ref_from_bytes(raw).map_err(|_| error())?;
            let end = u64::from(section.offset.get())
                + u64::from(section.count.get()) * u64::from(section.stride.get());
            if end > bytes.len() as u64
                || (section.kind.get() == Self::CONTENT_INDEX
                    && section.stride.get() != Self::RECORD_SIZE)
            {
                return Err(error());
            }
        }
        Ok(())
    }
}
