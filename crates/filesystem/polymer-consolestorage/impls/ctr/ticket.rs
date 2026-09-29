use crate::Error;
use crate::impls::es::{RightsHeader, TicketBody, TitleKey};
use crate::keys::KeyRecord;
use csis::{CiaArchive, CiaSection};
use std::path::Path;
use zerocopy::{FromBytes, IntoBytes};

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
        let (body, index) = bytes
            .get(offset..)
            .and_then(|bytes| TicketBody::ref_from_prefix(bytes).ok())
            .ok_or_else(|| Error::malformed("3DS ticket", "truncated body"))?;
        let (rights, _) = RightsHeader::ref_from_prefix(index)
            .ok()
            .filter(|_| body.version == 1)
            .ok_or_else(|| {
                Error::malformed("3DS ticket", "invalid version or truncated content index")
            })?;
        let index_size = rights.size.get() as usize;
        if index_size < size_of::<RightsHeader>() || index_size > index.len() {
            return Err(Error::malformed(
                "3DS ticket",
                "content index exceeds ticket",
            ));
        }
        let common = match (body.issuer_name(), body.common_key_index) {
            (b"Root-CA00000003-XS0000000c", index) => RETAIL_COMMON_KEYS.get(usize::from(index)),
            (b"Root-CA00000004-XS00000009", 0) => Some(&DEV_COMMON_KEY),
            _ => None,
        }
        .ok_or_else(|| Error::Unsupported("3DS ticket common key is unavailable".into()))?;
        if body.device_id.get() != 0 {
            return Err(Error::Unsupported(
                "personalized 3DS ticket requires depersonalization".into(),
            ));
        }
        let title_id = body.title_id.get();
        let key = TitleKey::decrypt(common, title_id, &body.encrypted_title_key);
        Ok(KeyRecord::content(
            format!("{title_id:016X}"),
            key.0.to_vec(),
        ))
    }

    pub fn synthesize(title_id: u64, key: &[u8]) -> Result<Vec<u8>, Error> {
        let key = TitleKey::new(key, "3DS title key")?;
        let mut body = TicketBody::issued(
            b"Root-CA00000003-XS0000000c",
            title_id,
            key.encrypt(&RETAIL_COMMON_KEYS[0], title_id),
        );
        body.version = 1;
        let mut bytes = vec![0u8; 0x140];
        bytes[..4].copy_from_slice(&0x10001u32.to_be_bytes());
        bytes.extend_from_slice(body.as_bytes());
        bytes.extend_from_slice(&RightsHeader::synthesize(64));
        Ok(bytes)
    }
}
