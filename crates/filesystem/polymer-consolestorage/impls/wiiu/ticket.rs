use crate::Error;
use crate::impls::es::{RightsHeader, TicketBody, TitleKey};
use crate::keys::KeyRecord;
use zerocopy::{FromBytes, IntoBytes};

const COMMON_KEY: [u8; 16] = [
    0xd7, 0xb0, 0x04, 0x02, 0x65, 0x9b, 0xa2, 0xab, 0xd2, 0xcb, 0x0d, 0xb2, 0x7f, 0xa2, 0xb6, 0x56,
];
const DEV_COMMON_KEY: [u8; 16] = [
    0x2f, 0x5c, 0x1b, 0x29, 0x44, 0xe7, 0xfd, 0x6f, 0xc3, 0x97, 0x96, 0x4b, 0x05, 0x76, 0x91, 0xfa,
];

pub struct WiiUTicket;

impl WiiUTicket {
    pub fn decode(bytes: &[u8]) -> Result<KeyRecord, Error> {
        let (body, rights) = bytes
            .get(0x140..)
            .and_then(|bytes| TicketBody::ref_from_prefix(bytes).ok())
            .ok_or_else(|| Error::malformed("Wii U ticket", "truncated ticket"))?;
        if !matches!(
            u32::from_be_bytes(bytes[..4].try_into().unwrap()),
            0x0001_0001 | 0x0001_0004
        ) {
            return Err(Error::malformed(
                "Wii U ticket",
                "unsupported signature type",
            ));
        }
        if body.version > 1 {
            return Err(Error::malformed(
                "Wii U ticket",
                "unsupported ticket version",
            ));
        }
        if body.version == 1 {
            RightsHeader::validate(rights, "Wii U ticket")?;
        }
        if body.common_key_index != 0 {
            return Err(Error::Unsupported(
                "Wii U ticket common key index is unavailable".into(),
            ));
        }
        let title_id = body.title_id.get();
        if title_id >> 48 != 5 {
            return Err(Error::malformed("Wii U ticket", "invalid title ID"));
        }
        if body.device_id.get() != 0 {
            return Err(Error::Unsupported(
                "personalized Wii U ticket requires depersonalization".into(),
            ));
        }
        let common = match body.issuer_name() {
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
        let key = TitleKey::decrypt(common, title_id, &body.encrypted_title_key);
        Ok(KeyRecord::content(
            format!("{title_id:016x}"),
            key.0.to_vec(),
        ))
    }

    pub fn synthesize(title_id: u64, title_version: u16, key: &[u8]) -> Result<Vec<u8>, Error> {
        let key = TitleKey::new(key, "Wii U title key")?;
        let dlc = (title_id >> 32) as u32 == 0x0005_000c;
        let mut body = TicketBody::issued(
            b"Root-CA00000003-XS0000000c",
            title_id,
            key.encrypt(&COMMON_KEY, title_id),
        );
        body.version = u8::from(dlc);
        body.title_version.set(title_version);
        body.content_access.fill(0xff);
        let signature: u32 = if dlc { 0x0001_0004 } else { 0x0001_0001 };
        let mut bytes = vec![0u8; 0x140];
        bytes[..4].copy_from_slice(&signature.to_be_bytes());
        bytes.extend_from_slice(body.as_bytes());
        if dlc {
            bytes.extend_from_slice(&RightsHeader::synthesize(1));
        } else {
            bytes.extend_from_slice(&[0; size_of::<RightsHeader>()]);
        }
        Ok(bytes)
    }
}
