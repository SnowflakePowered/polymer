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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correlates_responses_and_rejects_naks_for_each_transport() {
        for transport in [0, 1] {
            let request = CommandPacket::new(0x15, 1, transport);
            let mut response = [
                0x15,
                1,
                transport,
                1,
                0x10,
                if transport == 0 { 0xf8 } else { 0x78 },
                0,
                0,
                1,
            ];
            assert_eq!(request.response(&response).unwrap().unwrap().as_ref(), &[1]);
            response[3] = 4;
            assert!(request.response(&response).unwrap().is_none());
            response[3] = 1;
            response[2] ^= 1;
            assert!(request.response(&response).unwrap().is_none());
            response[2] = transport;
            response[5] = 0;
            assert!(request.response(&response).is_err());
            assert!(request.response(&response[..7]).unwrap().is_none());
        }
    }
}
