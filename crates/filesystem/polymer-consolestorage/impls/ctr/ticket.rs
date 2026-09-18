use crate::{
    Error,
    keys::{KeyIdentity, KeyRecord},
};
use aes::Aes128;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};
use csis::{CiaArchive, CiaSection};
use std::path::Path;

const RETAIL_COMMON_KEYS: [[u8; 16]; 6] = [
    [
        0x64, 0xc5, 0xfd, 0x55, 0xdd, 0x3a, 0xd9, 0x88, 0x32, 0x5b, 0xaa, 0xec, 0x52, 0x43, 0xdb,
        0x98,
    ],
    [
        0x4a, 0xaa, 0x3d, 0x0e, 0x27, 0xd4, 0xd7, 0x28, 0xd0, 0xb1, 0xb4, 0x33, 0xf0, 0xf9, 0xcb,
        0xc8,
    ],
    [
        0xfb, 0xb0, 0xef, 0x8c, 0xdb, 0xb0, 0xd8, 0xe4, 0x53, 0xcd, 0x99, 0x34, 0x43, 0x71, 0x69,
        0x7f,
    ],
    [
        0x25, 0x95, 0x9b, 0x7a, 0xd0, 0x40, 0x9f, 0x72, 0x68, 0x41, 0x98, 0xba, 0x2e, 0xcd, 0x7d,
        0xc6,
    ],
    [
        0x7a, 0xda, 0x22, 0xca, 0xff, 0xc4, 0x76, 0xcc, 0x82, 0x97, 0xa0, 0xc7, 0xce, 0xee, 0xee,
        0xbe,
    ],
    [
        0xa5, 0x05, 0x1c, 0xa1, 0xb3, 0x7d, 0xcf, 0x3a, 0xfb, 0xcf, 0x8c, 0xc1, 0xed, 0xd9, 0xce,
        0x02,
    ],
];
const DEV_COMMON_KEY: [u8; 16] = [
    0x55, 0xa3, 0xf8, 0x72, 0xbd, 0xc8, 0x0c, 0x55, 0x5a, 0x65, 0x43, 0x81, 0x13, 0x9e, 0x15, 0x3b,
];

pub struct CtrTicket;

impl CtrTicket {
    pub fn from_cia(path: &Path) -> Result<KeyRecord, Error> {
        let mut archive =
            CiaArchive::open(path).map_err(|e| Error::malformed("CIA", e.to_string()))?;
        let mut bytes = Vec::new();
        archive
            .stream_section(CiaSection::Ticket, &mut bytes)
            .map_err(|e| Error::malformed("CIA ticket", e.to_string()))?;
        Self::decode(&bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<KeyRecord, Error> {
        let signature = u32::from_be_bytes(
            *bytes
                .first_chunk()
                .ok_or_else(|| Error::malformed("3DS ticket", "missing signature"))?,
        );
        let offset = match signature {
            0x10000 | 0x10003 => 0x240,
            0x10001 | 0x10004 => 0x140,
            0x10002 | 0x10005 => 0x80,
            _ => return Err(Error::malformed("3DS ticket", "invalid signature layout")),
        };
        let body = bytes
            .get(offset..offset + 0x164)
            .ok_or_else(|| Error::malformed("3DS ticket", "truncated body"))?;
        let index = bytes
            .get(offset + 0x164..)
            .ok_or_else(|| Error::malformed("3DS ticket", "missing content index"))?;
        if body[0x7c] != 1 || index.len() < 0x14 {
            return Err(Error::malformed(
                "3DS ticket",
                "invalid version or truncated content index",
            ));
        }
        let index_size = u32::from_be_bytes(index[4..8].try_into().unwrap()) as usize;
        if index_size < 0x14 || index_size > index.len() {
            return Err(Error::malformed(
                "3DS ticket",
                "content index exceeds ticket",
            ));
        }
        let issuer = body[..0x40]
            .split(|byte| *byte == 0)
            .next()
            .unwrap_or_default();
        let common = match (issuer, body[0xb1]) {
            (b"Root-CA00000003-XS0000000c", index) => RETAIL_COMMON_KEYS.get(usize::from(index)),
            (b"Root-CA00000004-XS00000009", 0) => Some(&DEV_COMMON_KEY),
            _ => None,
        }
        .ok_or_else(|| Error::Unsupported("3DS ticket common key is unavailable".into()))?;
        if u32::from_be_bytes(body[0x98..0x9c].try_into().unwrap()) != 0 {
            return Err(Error::Unsupported(
                "personalized 3DS ticket requires depersonalization".into(),
            ));
        }
        let title_id = u64::from_be_bytes(body[0x9c..0xa4].try_into().unwrap());
        let mut key = GenericArray::clone_from_slice(&body[0x7f..0x8f]);
        Aes128::new(GenericArray::from_slice(common)).decrypt_block(&mut key);
        for (plain, iv) in key[..8].iter_mut().zip(title_id.to_be_bytes()) {
            *plain ^= iv;
        }
        let id = format!("{title_id:016X}");
        Ok(KeyRecord {
            identity: KeyIdentity {
                namespace: "content".into(),
                id: id.clone(),
            },
            title_id: Some(id),
            bytes: key.to_vec(),
        })
    }

    pub fn synthesize(title_id: u64, key: &[u8]) -> Result<Vec<u8>, Error> {
        let key: [u8; 16] = key
            .try_into()
            .map_err(|_| Error::malformed("3DS title key", "expected 16 bytes"))?;
        const RIGHTS_COUNT: usize = 64;
        const RIGHTS_SIZE: usize = 0x28 + 0x84 * RIGHTS_COUNT;
        let mut bytes = vec![0u8; 0x140 + 0x164 + RIGHTS_SIZE];
        bytes[..4].copy_from_slice(&0x10001u32.to_be_bytes());
        let body = &mut bytes[0x140..];
        let issuer = b"Root-CA00000003-XS0000000c";
        body[..issuer.len()].copy_from_slice(issuer);
        body[0x7c] = 1;
        body[0x90..0x98].copy_from_slice(&title_id.to_be_bytes());
        body[0x9c..0xa4].copy_from_slice(&title_id.to_be_bytes());
        body[0xb1] = 0;
        let mut block = GenericArray::clone_from_slice(&key);
        for (plain, iv) in block[..8].iter_mut().zip(title_id.to_be_bytes()) {
            *plain ^= iv;
        }
        Aes128::new(GenericArray::from_slice(&RETAIL_COMMON_KEYS[0])).encrypt_block(&mut block);
        body[0x7f..0x8f].copy_from_slice(&block);
        let rights = &mut body[0x164..];
        rights[..2].copy_from_slice(&1u16.to_be_bytes());
        rights[2..4].copy_from_slice(&0x14u16.to_be_bytes());
        rights[4..8].copy_from_slice(&(RIGHTS_SIZE as u32).to_be_bytes());
        rights[8..12].copy_from_slice(&0x14u32.to_be_bytes());
        rights[12..14].copy_from_slice(&1u16.to_be_bytes());
        rights[14..16].copy_from_slice(&0x14u16.to_be_bytes());
        rights[20..24].copy_from_slice(&0x28u32.to_be_bytes());
        rights[24..28].copy_from_slice(&(RIGHTS_COUNT as u32).to_be_bytes());
        rights[28..32].copy_from_slice(&0x84u32.to_be_bytes());
        rights[32..36].copy_from_slice(&((RIGHTS_COUNT * 0x84) as u32).to_be_bytes());
        rights[36..38].copy_from_slice(&3u16.to_be_bytes());
        for index in 0..RIGHTS_COUNT {
            let start = 0x28 + 0x84 * index;
            rights[start + 2..start + 4].copy_from_slice(&((index * 1024) as u16).to_be_bytes());
            rights[start + 4..start + 0x84].fill(0xff);
        }
        Ok(bytes)
    }
}
