use bytes::{Bytes, BytesMut};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned, byteorder::little_endian::U16,
};

use crate::{Error, Result};

pub const SIGNALING_CID: u16 = 0x0001;
pub const FIRST_DYNAMIC_CID: u16 = 0x0040;

#[derive(FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Header {
    length: U16,
    channel_id: U16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Packet {
    pub channel_id: u16,
    pub payload: Bytes,
}

impl Packet {
    pub fn new(channel_id: u16, payload: impl Into<Bytes>) -> Self {
        Self {
            channel_id,
            payload: payload.into(),
        }
    }

    pub fn parse(pdu: &[u8]) -> Result<Self> {
        let (header, data) =
            Header::ref_from_prefix(pdu).map_err(|_| Error::InvalidPacket("short L2CAP header"))?;
        let payload = data
            .get(..usize::from(header.length.get()))
            .ok_or(Error::InvalidPacket("truncated L2CAP payload"))?;
        Ok(Self {
            channel_id: header.channel_id.get(),
            payload: Bytes::copy_from_slice(payload),
        })
    }

    pub fn encode(&self) -> Bytes {
        assert!(
            self.payload.len() <= u16::MAX as usize,
            "L2CAP payload is too large"
        );
        let mut pdu = BytesMut::with_capacity(4 + self.payload.len());
        pdu.extend_from_slice(
            Header {
                length: U16::new(self.payload.len() as u16),
                channel_id: U16::new(self.channel_id),
            }
            .as_bytes(),
        );
        pdu.extend_from_slice(&self.payload);
        pdu.freeze()
    }
}
