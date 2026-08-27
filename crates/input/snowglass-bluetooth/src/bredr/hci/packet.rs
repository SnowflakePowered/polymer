use std::collections::HashMap;

use bytes::{BufMut, Bytes, BytesMut};

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum BoundaryFlag {
    FirstNonFlushable = 0,
    Continuing = 1,
    FirstAutomaticallyFlushable = 2,
    Complete = 3,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AclPacket {
    pub handle: u16,
    pub boundary: BoundaryFlag,
    pub broadcast: u8,
    pub data: Bytes,
}

impl AclPacket {
    pub fn parse(packet: &[u8]) -> Result<Self> {
        if packet.len() < 4 {
            return Err(Error::InvalidPacket("short ACL header"));
        }
        let flags = u16::from_le_bytes([packet[0], packet[1]]);
        let len = u16::from_le_bytes([packet[2], packet[3]]) as usize;
        if packet.len() < 4 + len {
            return Err(Error::InvalidPacket("truncated ACL data"));
        }
        let boundary = match (flags >> 12) & 3 {
            0 => BoundaryFlag::FirstNonFlushable,
            1 => BoundaryFlag::Continuing,
            2 => BoundaryFlag::FirstAutomaticallyFlushable,
            _ => BoundaryFlag::Complete,
        };
        Ok(Self {
            handle: flags & 0x0fff,
            boundary,
            broadcast: ((flags >> 14) & 3) as u8,
            data: Bytes::copy_from_slice(&packet[4..4 + len]),
        })
    }

    pub fn encode(&self) -> Bytes {
        let mut out = BytesMut::with_capacity(4 + self.data.len());
        let flags = (self.handle & 0x0fff)
            | ((self.boundary as u16) << 12)
            | (u16::from(self.broadcast & 3) << 14);
        out.put_u16_le(flags);
        out.put_u16_le(self.data.len() as u16);
        out.extend_from_slice(&self.data);
        out.freeze()
    }

    pub fn fragment(
        handle: u16,
        payload: &[u8],
        acl_data_len: usize,
        flushable: bool,
    ) -> Result<Vec<Self>> {
        if acl_data_len == 0 {
            return Err(Error::InvalidPacket(
                "controller reported zero ACL packet length",
            ));
        }
        let mut packets = Vec::new();
        for (index, chunk) in payload.chunks(acl_data_len).enumerate() {
            packets.push(Self {
                handle: handle & 0x0fff,
                boundary: if index == 0 {
                    if flushable {
                        BoundaryFlag::FirstAutomaticallyFlushable
                    } else {
                        BoundaryFlag::FirstNonFlushable
                    }
                } else {
                    BoundaryFlag::Continuing
                },
                broadcast: 0,
                data: Bytes::copy_from_slice(chunk),
            });
        }
        Ok(packets)
    }
}

#[derive(Default)]
pub struct AclReassembler {
    partial: HashMap<u16, Partial>,
}

struct Partial {
    expected: usize,
    data: BytesMut,
}

impl AclReassembler {
    /// Returns a complete L2CAP PDU (including its four-byte header).
    pub fn push(&mut self, packet: AclPacket) -> Result<Option<Bytes>> {
        match packet.boundary {
            BoundaryFlag::FirstNonFlushable
            | BoundaryFlag::FirstAutomaticallyFlushable
            | BoundaryFlag::Complete => {
                if packet.data.len() < 4 {
                    return Err(Error::InvalidPacket("ACL start lacks L2CAP header"));
                }
                let expected = 4 + u16::from_le_bytes([packet.data[0], packet.data[1]]) as usize;
                if packet.data.len() > expected {
                    return Err(Error::InvalidPacket("ACL data exceeds L2CAP length"));
                }
                if packet.data.len() == expected {
                    self.partial.remove(&packet.handle);
                    return Ok(Some(packet.data));
                }
                self.partial.insert(
                    packet.handle,
                    Partial {
                        expected,
                        data: BytesMut::from(&packet.data[..]),
                    },
                );
                Ok(None)
            }
            BoundaryFlag::Continuing => {
                let partial = self
                    .partial
                    .get_mut(&packet.handle)
                    .ok_or(Error::InvalidPacket("orphan ACL continuation"))?;
                if partial.data.len() + packet.data.len() > partial.expected {
                    self.partial.remove(&packet.handle);
                    return Err(Error::InvalidPacket(
                        "ACL continuation exceeds L2CAP length",
                    ));
                }
                partial.data.extend_from_slice(&packet.data);
                if partial.data.len() == partial.expected {
                    return Ok(Some(
                        self.partial.remove(&packet.handle).unwrap().data.freeze(),
                    ));
                }
                Ok(None)
            }
        }
    }

    pub fn discard_handle(&mut self, handle: u16) {
        self.partial.remove(&(handle & 0x0fff));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_and_reassembles_l2cap() {
        let payload = [8, 0, 0x41, 0, 1, 2, 3, 4, 5, 6, 7, 8];
        let fragments = AclPacket::fragment(0x123, &payload, 5, true).unwrap();
        assert_eq!(fragments.len(), 3);
        let mut rx = AclReassembler::default();
        assert!(rx.push(fragments[0].clone()).unwrap().is_none());
        assert!(rx.push(fragments[1].clone()).unwrap().is_none());
        assert_eq!(
            &rx.push(fragments[2].clone()).unwrap().unwrap()[..],
            &payload
        );
    }
}
