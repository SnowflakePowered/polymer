use crate::{
    Error,
    keys::{KeyIdentity, KeyRecord},
};
use aes::Aes128;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};

const COMMON_KEY: [u8; 16] = [
    0xd7, 0xb0, 0x04, 0x02, 0x65, 0x9b, 0xa2, 0xab, 0xd2, 0xcb, 0x0d, 0xb2, 0x7f, 0xa2, 0xb6, 0x56,
];
const DEV_COMMON_KEY: [u8; 16] = [
    0x2f, 0x5c, 0x1b, 0x29, 0x44, 0xe7, 0xfd, 0x6f, 0xc3, 0x97, 0x96, 0x4b, 0x05, 0x76, 0x91, 0xfa,
];

pub struct WiiUTicket;

impl WiiUTicket {
    pub fn decode(bytes: &[u8]) -> Result<KeyRecord, Error> {
        if bytes.len() < 0x2a4 {
            return Err(Error::malformed("Wii U ticket", "truncated ticket"));
        }
        if !matches!(
            u32::from_be_bytes(bytes[..4].try_into().unwrap()),
            0x0001_0001 | 0x0001_0004
        ) {
            return Err(Error::malformed(
                "Wii U ticket",
                "unsupported signature type",
            ));
        }
        if bytes[0x1bc] > 1 {
            return Err(Error::malformed(
                "Wii U ticket",
                "unsupported ticket version",
            ));
        }
        if bytes[0x1bc] == 1 {
            Self::validate_rights(&bytes[0x2a4..])?;
        }
        if bytes[0x1f1] != 0 {
            return Err(Error::Unsupported(
                "Wii U ticket common key index is unavailable".into(),
            ));
        }
        let title_id = u64::from_be_bytes(bytes[0x1dc..0x1e4].try_into().unwrap());
        if title_id >> 48 != 5 {
            return Err(Error::malformed("Wii U ticket", "invalid title ID"));
        }
        if bytes[0x1d8..0x1dc] != [0; 4] {
            return Err(Error::Unsupported(
                "personalized Wii U ticket requires depersonalization".into(),
            ));
        }
        let issuer = bytes[0x140..0x180]
            .split(|byte| *byte == 0)
            .next()
            .unwrap_or_default();
        let key = match issuer {
            b"Root-CA00000003-XS0000000c" => &COMMON_KEY,
            b"Root-CA00000004-XS00000009"
            | b"Root-CA00000002-XS00000004"
            | b"Root-CA00000002-XS00000006" => &DEV_COMMON_KEY,
            _ => {
                return Err(Error::Unsupported(
                    "Wii U ticket issuer is unavailable".into(),
                ));
            }
        };
        let mut block = GenericArray::clone_from_slice(&bytes[0x1bf..0x1cf]);
        Aes128::new(GenericArray::from_slice(key)).decrypt_block(&mut block);
        for (plain, iv) in block[..8].iter_mut().zip(title_id.to_be_bytes()) {
            *plain ^= iv;
        }
        let id = format!("{title_id:016x}");
        Ok(KeyRecord {
            identity: KeyIdentity {
                namespace: "content".into(),
                id: id.clone(),
            },
            title_id: Some(id),
            bytes: block.to_vec(),
        })
    }

    fn validate_rights(bytes: &[u8]) -> Result<(), Error> {
        use zerocopy::byteorder::big_endian::{U16, U32};
        use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};
        #[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
        #[repr(C)]
        struct Header {
            version: U16,
            header_size: U16,
            size: U32,
            offset: U32,
            count: U16,
            stride: U16,
            reserved: U32,
        }
        #[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
        #[repr(C)]
        struct Section {
            offset: U32,
            count: U32,
            stride: U32,
            size: U32,
            kind: U16,
            reserved: U16,
        }
        let error = || Error::malformed("Wii U ticket", "invalid or truncated content rights");
        let (header, _) = Header::ref_from_prefix(bytes).map_err(|_| error())?;
        if header.version.get() != 1 || header.header_size.get() != 20 || header.stride.get() != 20
        {
            return Err(error());
        }
        let table_start = u64::from(header.offset.get());
        let table_end = table_start + u64::from(header.count.get()) * 20;
        let table = bytes
            .get(table_start as usize..usize::try_from(table_end).map_err(|_| error())?)
            .ok_or_else(error)?;
        for raw in table.chunks_exact(20) {
            let section = Section::ref_from_bytes(raw).map_err(|_| error())?;
            let end = u64::from(section.offset.get())
                + u64::from(section.count.get()) * u64::from(section.stride.get());
            if end > bytes.len() as u64 || (section.kind.get() == 3 && section.stride.get() != 0x84)
            {
                return Err(error());
            }
        }
        Ok(())
    }

    pub fn synthesize(title_id: u64, title_version: u16, key: &[u8]) -> Result<Vec<u8>, Error> {
        let key: [u8; 16] = key
            .try_into()
            .map_err(|_| Error::malformed("Wii U title key", "expected 16 bytes"))?;
        let dlc = (title_id >> 32) as u32 == 0x0005_000c;
        let mut bytes = vec![
            0u8;
            if dlc {
                0x2a4 + 20 + 20 + 0x84
            } else {
                0x2a4 + 20
            }
        ];
        let signature: u32 = if dlc { 0x0001_0004 } else { 0x0001_0001 };
        bytes[..4].copy_from_slice(&signature.to_be_bytes());
        let issuer = b"Root-CA00000003-XS0000000c";
        bytes[0x140..0x140 + issuer.len()].copy_from_slice(issuer);
        bytes[0x1bc] = u8::from(dlc);
        bytes[0x1d0..0x1d8].copy_from_slice(&title_id.to_be_bytes());
        bytes[0x1dc..0x1e4].copy_from_slice(&title_id.to_be_bytes());
        bytes[0x1e6..0x1e8].copy_from_slice(&title_version.to_be_bytes());
        let mut block = GenericArray::clone_from_slice(&key);
        for (plain, iv) in block[..8].iter_mut().zip(title_id.to_be_bytes()) {
            *plain ^= iv;
        }
        Aes128::new(GenericArray::from_slice(&COMMON_KEY)).encrypt_block(&mut block);
        bytes[0x1bf..0x1cf].copy_from_slice(&block);
        bytes[0x222..0x262].fill(0xff);
        if dlc {
            let extension_length = (bytes.len() - 0x2a4) as u32;
            let ext = &mut bytes[0x2a4..];
            ext[..2].copy_from_slice(&1u16.to_be_bytes());
            ext[2..4].copy_from_slice(&20u16.to_be_bytes());
            ext[4..8].copy_from_slice(&extension_length.to_be_bytes());
            ext[8..12].copy_from_slice(&20u32.to_be_bytes());
            ext[12..14].copy_from_slice(&1u16.to_be_bytes());
            ext[14..16].copy_from_slice(&20u16.to_be_bytes());
            ext[20..24].copy_from_slice(&40u32.to_be_bytes());
            ext[24..28].copy_from_slice(&1u32.to_be_bytes());
            ext[28..32].copy_from_slice(&0x84u32.to_be_bytes());
            ext[32..36].copy_from_slice(&0x84u32.to_be_bytes());
            ext[36..38].copy_from_slice(&3u16.to_be_bytes());
            ext[44..].fill(0xff);
        }
        Ok(bytes)
    }
}
