//! DualSense Bluetooth wire format, excluding HIDP transaction headers.
//! Protocol reference: Sony's Linux drivers/hid/hid-playstation.c.
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned, byteorder::little_endian::I16};

pub(crate) use super::super::protocol::Report;
pub use super::super::protocol::TouchContact;

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct InputReport {
    id: u8,
    tag: u8,
    pub sticks: [u8; 4],
    pub triggers: [u8; 2],
    sequence: u8,
    pub buttons: [u8; 4],
    reserved: [u8; 4],
    pub gyroscope: [I16; 3],
    pub accelerometer: [I16; 3],
    pub timestamp: [u8; 4],
    reserved2: u8,
    pub touch: [TouchContact; 2],
    reserved3: [u8; 12],
    pub status: [u8; 3],
    reserved4: [u8; 17],
    crc: [u8; 4],
}

const _: () = assert!(std::mem::size_of::<InputReport>() == 78);

impl InputReport {
    pub fn parse(data: &[u8]) -> Option<&Self> {
        if !Report::valid(data, 0x31, 78, 0xa1) {
            return None;
        }
        Self::ref_from_bytes(data).ok()
    }
}

#[derive(Clone, Default)]
pub(super) struct Output {
    pub player: u8,
    pub low: u16,
    pub high: u16,
    pub sequence: u8,
}

impl Output {
    pub fn report(&self, reset_leds: bool) -> [u8; 78] {
        let mut data = [0; 78];
        data[0] = 0x31;
        data[1] = (self.sequence & 15) << 4;
        data[2] = 0x10;
        if reset_leds {
            data[41] = 2; // lightbar setup valid
            data[44] = 2; // release startup animation
        } else {
            data[3] = 3; // compatible vibration + haptics select
            data[4] = 0x14; // lightbar and player indicators valid
            // Legacy compatible vibration uses half the motor range (SDL).
            data[5] = (self.high >> 9) as u8;
            data[6] = (self.low >> 9) as u8;
            data[46] = [4, 10, 21, 27, 31][usize::from(self.player % 5)];
            data[49] = 128; // blue lightbar
        }
        let crc = Report::checksum(0xa2, &data[..74]);
        data[74..].copy_from_slice(&crc.to_le_bytes());
        data
    }
}
