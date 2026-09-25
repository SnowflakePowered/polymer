use zerocopy::byteorder::big_endian::{U32, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::depots::wad::crypto::COMMON_KEYS;
use super::depots::wad::signed_body;
use crate::Error;
use crate::keys::{KeyIdentity, KeyRecord};
use aes::Aes128;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit, generic_array::GenericArray};

const COMMON_KEY_INDEX: usize = 0xb1;

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct TicketBody {
    before_title_key: [u8; 0x7f],
    encrypted_title_key: [u8; 16],
    before_device_id: [u8; 9],
    device_id: U32,
    title_id: U64,
    before_common_key: [u8; 13],
    common_key_index: u8,
}

const _: () = assert!(size_of::<TicketBody>() == COMMON_KEY_INDEX + 1);

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
        let mut block = GenericArray::clone_from_slice(&self.encrypted_title_key);
        Aes128::new(GenericArray::from_slice(common)).decrypt_block(&mut block);
        for (plain, iv) in block[..8].iter_mut().zip(self.title_id.to_be_bytes()) {
            *plain ^= iv;
        }
        let title_id = format!("{:016x}", self.title_id);
        Ok(KeyRecord {
            identity: KeyIdentity {
                namespace: "content".into(),
                id: title_id.clone(),
            },
            title_id: Some(title_id),
            bytes: block.to_vec(),
        })
    }

    pub fn synthesize(title_id: u64, key: &[u8]) -> Result<Vec<u8>, Error> {
        let key: [u8; 16] = key
            .try_into()
            .map_err(|_| Error::malformed("Wii title key", "expected 16 bytes"))?;
        let mut bytes = vec![0u8; 0x2a4];
        bytes[..4].copy_from_slice(&0x0001_0001u32.to_be_bytes());
        let body = &mut bytes[0x140..];
        let issuer = b"Root-CA00000001-XS00000003";
        body[..issuer.len()].copy_from_slice(issuer);
        body[0x90..0x98].copy_from_slice(&title_id.to_be_bytes());
        body[0x9c..0xa4].copy_from_slice(&title_id.to_be_bytes());
        let mut block = GenericArray::clone_from_slice(&key);
        for (plain, iv) in block[..8].iter_mut().zip(title_id.to_be_bytes()) {
            *plain ^= iv;
        }
        Aes128::new(GenericArray::from_slice(&COMMON_KEYS[0])).encrypt_block(&mut block);
        body[0x7f..0x8f].copy_from_slice(&block);
        body[0xb1] = 0;
        body[0xe2..0x122].fill(0xff);
        Ok(bytes)
    }
    pub fn parse(bytes: Vec<u8>) -> Result<Self, Error> {
        let (signature_size, body) = signed_body(&bytes, "ticket")?;
        if body.len() < 0x164 {
            return Err(Error::malformed("ticket", "ticket body is truncated"));
        }
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
            common_key_index: body.common_key_index,
            fakesigned: signature_bytes.iter().all(|byte| *byte == 0),
        })
    }
}
