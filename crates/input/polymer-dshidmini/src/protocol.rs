use std::io;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

/// DsHidMini sys/IPC.h v3.3.1187.0; enums and UINT32s are little endian.
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable)]
#[repr(C)]
pub(crate) struct Header {
    kind: u32,
    target: u32,
    command: u32,
    slot: u32,
    size: u32,
}

const _: () = assert!(std::mem::size_of::<Header>() == 20);

impl Header {
    pub fn pair_request(slot: u32, address: [u8; 6]) -> [u8; 28] {
        // BD_ADDR is six bytes; the native struct has two trailing pad bytes.
        let header = Self {
            kind: 2,
            target: 2,
            command: 1,
            slot,
            size: 28,
        };
        let mut request = [0; 28];
        request[..20].copy_from_slice(header.as_bytes());
        request[20..26].copy_from_slice(&address);
        request
    }

    pub fn validate_reply(bytes: &[u8], slot: u32) -> io::Result<()> {
        let (header, _) =
            Self::read_from_prefix(bytes).map_err(|_| io::Error::other("short DsHidMini reply"))?;
        if bytes.len() != 28
            || (
                header.kind,
                header.target,
                header.command,
                header.slot,
                header.size,
            ) != (4, 3, 1, slot, 28)
        {
            return Err(io::Error::other(
                "unexpected DsHidMini pairing reply; no verified pairing",
            ));
        }
        for (name, offset) in [("write", 20), ("readback", 24)] {
            let status = i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
            if status < 0 {
                return Err(io::Error::other(format!(
                    "DsHidMini pairing {name} failed: NTSTATUS 0x{:08X}",
                    status as u32
                )));
            }
        }
        Ok(())
    }

    pub fn legacy_slot(bytes: &[u8]) -> io::Result<u32> {
        // v3.3 packs UINT32 SlotIndex + 49-byte DS3 report without padding.
        let slots: Vec<_> = bytes
            .chunks_exact(53)
            .take(255)
            .enumerate()
            .filter_map(|(index, record)| {
                let slot = u32::from_le_bytes(record[..4].try_into().unwrap());
                (slot != 0).then_some((index as u32 + 1, slot))
            })
            .collect();
        match slots.as_slice() {
            [(expected, slot)] if expected == slot => Ok(*slot),
            _ => Err(io::Error::other(
                "DsHidMini legacy IPC needs exactly one active controller; disconnect other DsHidMini controllers and refresh",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_reply_rejects_wrong_target_and_failed_readback() {
        let mut reply = Header::pair_request(7, [1, 2, 3, 4, 5, 6]);
        reply[0..4].copy_from_slice(&4u32.to_le_bytes());
        reply[4..8].copy_from_slice(&3u32.to_le_bytes());
        reply[20..28].fill(0);
        assert!(Header::validate_reply(&reply, 7).is_ok());
        assert!(Header::validate_reply(&reply, 1).is_err());
        reply[24..28].copy_from_slice(&0xc0000001u32.to_le_bytes());
        assert!(Header::validate_reply(&reply, 7).is_err());
    }

    #[test]
    fn legacy_slot_uses_occupied_record_not_enumeration_order() {
        let mut records = vec![0; 53 * 255];
        records[53 * 4..53 * 4 + 4].copy_from_slice(&5u32.to_le_bytes());
        assert_eq!(Header::legacy_slot(&records).unwrap(), 5);
        records[..4].copy_from_slice(&1u32.to_le_bytes());
        assert!(Header::legacy_slot(&records).is_err());
    }
}
