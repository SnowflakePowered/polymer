//! Shared Sony HID checksums and packed touch contacts.
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

pub(crate) struct Report;

impl Report {
    pub fn checksum(seed: u8, data: &[u8]) -> u32 {
        let mut crc = crc32fast::Hasher::new();
        crc.update(&[seed]);
        crc.update(data);
        crc.finalize()
    }

    pub fn valid(data: &[u8], id: u8, size: usize, seed: u8) -> bool {
        if data.len() != size || size < 5 || data[0] != id {
            return false;
        }
        let end = size - 4;
        data[end..] == Self::checksum(seed, &data[..end]).to_le_bytes()
    }
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct TouchContact {
    status: u8,
    coordinates: [u8; 3],
}

impl TouchContact {
    pub fn active(&self) -> bool {
        self.status & 0x80 == 0
    }
    pub fn id(&self) -> u8 {
        self.status & 0x7f
    }
    pub fn position(&self) -> [u16; 2] {
        let [lo, shared, hi] = self.coordinates;
        [
            u16::from(lo) | (u16::from(shared & 15) << 8),
            u16::from(shared >> 4) | (u16::from(hi) << 4),
        ]
    }
}
