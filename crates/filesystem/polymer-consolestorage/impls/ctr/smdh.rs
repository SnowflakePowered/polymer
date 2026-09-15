//! Shared SMDH names and icons for installed titles and Azahar extdata.
use std::io::Read;

use image::{Rgba, RgbaImage};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned, byteorder::little_endian::U16};

use crate::{Storage, savegame::icons::SaveIcon};

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Title {
    short: [U16; 64],
    long: [U16; 128],
    publisher: [U16; 64],
}

impl Title {
    fn text(units: &[U16]) -> String {
        String::from_utf16_lossy(
            &units
                .iter()
                .map(|unit| unit.get())
                .take_while(|unit| *unit != 0)
                .collect::<Vec<_>>(),
        )
    }
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub(super) struct ExtdataIcon {
    magic: [u8; 4],
    version: U16,
    reserved: [u8; 2],
    titles: [Title; 16],
    settings: [u8; 0x38],
    small: [U16; 24 * 24],
    large: [U16; 48 * 48],
}

const _: () = assert!(size_of::<ExtdataIcon>() == 0x36c0);

impl ExtdataIcon {
    pub fn read(storage: &dyn Storage, root: &str) -> Option<Self> {
        let mut bytes = vec![0; size_of::<Self>()];
        storage
            .open_read(&format!("{root}/icon"))
            .ok()?
            .read_exact(&mut bytes)
            .ok()?;
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let icon = Self::read_from_bytes(bytes).ok()?;
        (icon.magic == *b"SMDH" && icon.version.get() == 0).then_some(icon)
    }

    pub fn description(&self) -> (String, String) {
        std::iter::once(&self.titles[1])
            .chain(self.titles.iter())
            .map(|title| (Title::text(&title.short), Title::text(&title.long)))
            .find(|(short, _)| !short.trim().is_empty())
            .unwrap_or_default()
    }

    pub fn rgba(&self) -> RgbaImage {
        RgbaImage::from_fn(48, 48, |x, y| {
            // 8x8 tiles, with Morton order within each tile.
            let local = (x & 1)
                | ((y & 1) << 1)
                | ((x & 2) << 1)
                | ((y & 2) << 2)
                | ((x & 4) << 2)
                | ((y & 4) << 3);
            let index = ((y / 8) * 6 + x / 8) * 64 + local;
            let pixel = self.large[index as usize].get();
            let r = ((pixel >> 11) & 31) as u8;
            let g = ((pixel >> 5) & 63) as u8;
            let b = (pixel & 31) as u8;
            Rgba([
                (r << 3) | (r >> 2),
                (g << 2) | (g >> 4),
                (b << 3) | (b >> 2),
                255,
            ])
        })
    }

    pub fn image(&self) -> SaveIcon {
        SaveIcon::Image(self.rgba())
    }
}
