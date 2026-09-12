//! DS4 Bluetooth reports, excluding HIDP headers.
//! Reference: Linux drivers/hid/hid-playstation.c and SDL_hidapi_ps4.c.
use super::super::protocol::{Report, TouchContact};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned, byteorder::little_endian::I16};

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct InputReport {
    id: u8,
    reserved: [u8; 2],
    pub sticks: [u8; 4],
    pub buttons: [u8; 3],
    pub triggers: [u8; 2],
    pub timestamp: [u8; 2],
    temperature: u8,
    pub gyroscope: [I16; 3],
    pub accelerometer: [I16; 3],
    reserved2: [u8; 5],
    pub status: [u8; 2],
    reserved3: u8,
    touch_count: u8,
    touch: [TouchReport; 4],
    reserved4: [u8; 2],
    crc: [u8; 4],
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct TouchReport {
    timestamp: u8,
    contacts: [TouchContact; 2],
}

const _: () = assert!(std::mem::size_of::<InputReport>() == 78);

impl InputReport {
    pub fn parse(data: &[u8]) -> Option<&Self> {
        if !Report::valid(data, 0x11, 78, 0xa1) {
            return None;
        }
        let report = Self::ref_from_bytes(data).ok()?;
        (report.touch_count <= 4).then_some(report)
    }

    /// Touch samples are chronological; zero means no new touch state.
    pub fn latest_touch(&self) -> Option<&[TouchContact; 2]> {
        self.touch_count
            .checked_sub(1)
            .map(|i| &self.touch[usize::from(i)].contacts)
    }
}

#[derive(Clone, Default)]
pub(super) struct Output {
    pub player: u8,
    pub low: u16,
    pub high: u16,
}

impl Output {
    pub fn report(&self) -> [u8; 78] {
        let mut data = [0; 78];
        data[0] = 0x11;
        data[1] = 0xc4; // HID + CRC, 4ms report interval.
        data[3] = 7; // Motors, lightbar and blink settings valid.
        data[6] = (self.high >> 8) as u8;
        data[7] = (self.low >> 8) as u8;
        data[8..11].copy_from_slice(
            &[[0, 0, 64], [64, 0, 0], [0, 64, 0], [32, 0, 32]][usize::from(self.player % 4)],
        );
        let crc = Report::checksum(0xa2, &data[..74]);
        data[74..].copy_from_slice(&crc.to_le_bytes());
        data
    }
}
