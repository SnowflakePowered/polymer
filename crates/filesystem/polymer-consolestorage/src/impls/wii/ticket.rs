use zerocopy::{FromBytes, IntoBytes};

use super::depots::wad::crypto::COMMON_KEYS;
use super::depots::wad::signed_body;
use crate::Error;
use crate::impls::es::{TicketBody, TitleKey};
use crate::keys::KeyRecord;

#[derive(Clone)]
pub struct Ticket {
    pub(crate) encrypted_title_key: [u8; 16],
    pub(crate) title_id: u64,
    pub(crate) common_key_index: u8,
    pub(crate) fakesigned: bool,
}

impl Ticket {
    pub fn key_record(&self) -> Result<KeyRecord, Error> {
        let common = COMMON_KEYS
            .get(usize::from(self.common_key_index))
            .ok_or_else(|| {
                Error::Unsupported(format!(
                    "Wii common key index {} is unavailable",
                    self.common_key_index
                ))
            })?;
        let key = TitleKey::decrypt(common, self.title_id, &self.encrypted_title_key);
        Ok(KeyRecord::content(
            format!("{:016x}", self.title_id),
            key.0.to_vec(),
        ))
    }

    pub fn synthesize(title_id: u64, key: &[u8]) -> Result<Vec<u8>, Error> {
        let key = TitleKey::new(key, "Wii title key")?;
        let mut body = TicketBody::issued(
            b"Root-CA00000001-XS00000003",
            title_id,
            key.encrypt(&COMMON_KEYS[0], title_id),
        );
        body.content_access.fill(0xff);
        let mut bytes = vec![0u8; 0x140];
        bytes[..4].copy_from_slice(&0x0001_0001u32.to_be_bytes());
        bytes.extend_from_slice(body.as_bytes());
        Ok(bytes)
    }

    pub fn parse(bytes: Vec<u8>) -> Result<Self, Error> {
        let (signature_size, body) = signed_body(&bytes, "ticket")?;
        let (body, _) = TicketBody::ref_from_prefix(body)
            .map_err(|_| Error::malformed("ticket", "ticket body is truncated"))?;
        if body.device_id.get() != 0 {
            return Err(Error::Unsupported(
                "console-personalized Wii tickets are not supported".to_owned(),
            ));
        }
        let signature_length = match signature_size {
            0x240 => 512,
            0x140 => 256,
            0x80 => 60,
            _ => return Err(Error::malformed("ticket", "invalid signature layout")),
        };
        let signature_bytes = bytes
            .get(4..4 + signature_length)
            .ok_or_else(|| Error::malformed("ticket", "ticket signature is truncated"))?;
        Ok(Self {
            encrypted_title_key: body.encrypted_title_key,
            title_id: body.title_id.get(),
            common_key_index: if body.common_key_index > 2 {
                0
            } else {
                body.common_key_index
            },
            fakesigned: signature_bytes.iter().all(|byte| *byte == 0),
        })
    }
}
