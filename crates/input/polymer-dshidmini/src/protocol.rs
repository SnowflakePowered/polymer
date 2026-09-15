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
