use bytes::{BufMut, Bytes, BytesMut};

use crate::{Error, Result};

pub const SIGNALING_CID: u16 = 0x0001;
pub const FIRST_DYNAMIC_CID: u16 = 0x0040;

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
        if pdu.len() < 4 {
            return Err(Error::InvalidPacket("short L2CAP header"));
        }
        let length = u16::from_le_bytes([pdu[0], pdu[1]]) as usize;
        if pdu.len() < length + 4 {
            return Err(Error::InvalidPacket("truncated L2CAP payload"));
        }
        Ok(Self {
            channel_id: u16::from_le_bytes([pdu[2], pdu[3]]),
            payload: Bytes::copy_from_slice(&pdu[4..4 + length]),
        })
    }

    pub fn encode(&self) -> Bytes {
        assert!(
            self.payload.len() <= u16::MAX as usize,
            "L2CAP payload is too large"
        );
        let mut pdu = BytesMut::with_capacity(4 + self.payload.len());
        pdu.put_u16_le(self.payload.len() as u16);
        pdu.put_u16_le(self.channel_id);
        pdu.extend_from_slice(&self.payload);
        pdu.freeze()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_round_trip() {
        let packet = Packet::new(0x41, Bytes::from_static(&[1, 2, 3]));
        assert_eq!(Packet::parse(&packet.encode()).unwrap(), packet);
    }
}
