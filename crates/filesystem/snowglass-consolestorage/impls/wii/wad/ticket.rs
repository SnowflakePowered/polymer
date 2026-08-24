use zerocopy::byteorder::big_endian::{U32, U64};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::signed_body;
use crate::Error;

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
pub(super) struct Ticket {
    pub(super) bytes: Vec<u8>,
    pub(super) encrypted_title_key: [u8; 16],
    pub(super) title_id: u64,
    pub(super) common_key_index: u8,
    pub(super) fakesigned: bool,
    common_key_offset: usize,
}

impl Ticket {
    pub(super) fn parse(bytes: Vec<u8>) -> Result<Self, Error> {
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
            common_key_index: body.common_key_index,
            fakesigned: signature_bytes.iter().all(|byte| *byte == 0),
            common_key_offset: signature_size + COMMON_KEY_INDEX,
            bytes,
        })
    }

    pub(super) fn bytes_with_key_index(&self, index: u8) -> Vec<u8> {
        let mut bytes = self.bytes.clone();
        bytes[self.common_key_offset] = index;
        bytes
    }
}
