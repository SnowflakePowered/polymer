use crate::{Error, Result};
use bytes::Bytes;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// Nintendo command header, distinct from the HID input report interface.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub(crate) struct CommandPacket {
    command: u8,
    direction: u8,
    transport: u8,
    subcommand: u8,
    unknown: u8,
    length_or_ack: u8,
    reserved: [u8; 2],
}

impl CommandPacket {
    pub fn new(command: u8, subcommand: u8, transport: u8) -> Self {
        Self {
            command,
            direction: 0x91,
            transport,
            subcommand,
            unknown: 0,
            length_or_ack: 0,
            reserved: [0; 2],
        }
    }

    pub fn encode(mut self, data: &[u8]) -> Result<Vec<u8>> {
        self.length_or_ack = u8::try_from(data.len())
            .map_err(|_| Error::InvalidPacket("Switch 2 command payload is too large"))?;
        let mut bytes = self.as_bytes().to_vec();
        bytes.extend_from_slice(data);
        Ok(bytes)
    }

    pub fn response(&self, bytes: &[u8]) -> Result<Option<Bytes>> {
        let Ok((header, data)) = Self::ref_from_prefix(bytes) else {
            return Ok(None);
        };
        if header.command != self.command
            || header.subcommand != self.subcommand
            || header.transport != self.transport
            || header.direction != 1
        {
            return Ok(None);
        }
        let ack = if self.transport == 0 { 0xf8 } else { 0x78 };
        if header.length_or_ack != ack {
            return Err(Error::InvalidPacket("Switch 2 command was rejected"));
        }
        Ok(Some(Bytes::copy_from_slice(data)))
    }
}
