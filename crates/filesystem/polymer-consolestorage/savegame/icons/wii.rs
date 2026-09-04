use std::io::Read;

use image::{Delay, Frame, RgbaImage};
use zerocopy::byteorder::big_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::{FromNintendoIcon, IconSave, SaveIcon};
use crate::Storage;
use crate::impls::wii::save_repo::Save;

const BANNER_PIXELS: usize = 192 * 64;
const ICON_PIXELS: usize = 48 * 48;
const MAX_ICONS: usize = 8;

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct BannerHeader {
    magic: [u8; 4],
    flags: U32,
    animation_speed: U16,
    unused: [u8; 22],
    title: [U16; 32],
    description: [U16; 32],
}

const _: () = assert!(size_of::<BannerHeader>() == 0xa0);

pub(crate) struct Banner {
    pub(crate) title: String,
    pub(crate) description: String,
}

impl Banner {
    pub(crate) fn read(storage: &dyn Storage, root: &str) -> Option<Self> {
        let path = format!("{root}/banner.bin");
        let mut bytes = Vec::new();
        storage
            .open_read(&path)
            .ok()?
            .read_to_end(&mut bytes)
            .ok()?;
        let (header, _) = BannerHeader::ref_from_prefix(&bytes).ok()?;
        if header.magic != *b"WIBN" {
            return None;
        }
        Some(Self {
            title: utf16be(&header.title),
            description: utf16be(&header.description),
        })
    }
}

impl IconSave for Save<'_> {
    fn read_icon(&self) -> Option<SaveIcon> {
        let mut bytes = Vec::new();
        self.storage
            .open_read(&format!("{}/banner.bin", self.root))
            .ok()?
            .read_to_end(&mut bytes)
            .ok()?;
        let (header, rest) = BannerHeader::ref_from_prefix(&bytes).ok()?;
        if header.magic != *b"WIBN" || rest.len() < BANNER_PIXELS * 2 + ICON_PIXELS * 2 {
            return None;
        }
        let icons = &rest[BANNER_PIXELS * 2..];
        let frame_count = (icons.len() / (ICON_PIXELS * 2)).min(MAX_ICONS);
        let speeds = header.animation_speed.get();
        let mut frames = Vec::new();
        for index in 0..frame_count {
            let speed = (speeds >> (index * 2)) & 3;
            if speed == 0 {
                continue;
            }
            let start = index * ICON_PIXELS * 2;
            let image = RgbaImage::from_rgb5a3(&icons[start..start + ICON_PIXELS * 2], 48, 48)?;
            frames.push(Frame::from_parts(
                image,
                0,
                0,
                Delay::from_numer_denom_ms(u32::from(speed) * 4_000, 60),
            ));
        }
        if header.flags.get() & 0x10 != 0 && frames.len() > 2 {
            let reverse = frames[1..frames.len() - 1]
                .iter()
                .rev()
                .cloned()
                .collect::<Vec<_>>();
            frames.extend(reverse);
        }
        match frames.len() {
            0 => None,
            1 => Some(SaveIcon::Image(frames.remove(0).into_buffer())),
            _ => Some(SaveIcon::Animation(frames)),
        }
    }
}

fn utf16be(value: &[U16]) -> String {
    String::from_utf16_lossy(
        &value
            .iter()
            .map(|unit| unit.get())
            .take_while(|unit| *unit != 0)
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::{PlatformRepository, Storage, StorageKind, VolumeStorage};

    #[tokio::test]
    async fn reads_banner_text_and_icon() {
        let directory = tempfile::tempdir().unwrap();
        let platform =
            PlatformRepository::open(directory.path().join("platform"), "NINTENDO_WII").unwrap();
        let id = Uuid::now_v7();
        let storage = platform.volume(StorageKind::Nand, id);
        storage.brand(id, "NINTENDO_WII", None).await.unwrap();
        let root = "title/00010000/524d4345/data";
        let mut banner = vec![0; 0xa0 + BANNER_PIXELS * 2 + ICON_PIXELS * 2];
        banner[..4].copy_from_slice(b"WIBN");
        banner[8..10].copy_from_slice(&1_u16.to_be_bytes());
        write_utf16(&mut banner[0x20..0x60], "Mario Kart Wii");
        write_utf16(&mut banner[0x60..0xa0], "License data");
        banner[0xa0 + BANNER_PIXELS * 2..].fill(0xff);
        storage
            .create(&format!("{root}/banner.bin"))
            .unwrap()
            .write_all_at(0, &banner)
            .unwrap();

        let parsed = Banner::read(&storage, root).unwrap();
        assert_eq!(parsed.title, "Mario Kart Wii");
        assert_eq!(parsed.description, "License data");
        let save = Save {
            storage: &storage,
            root: root.to_owned(),
        };
        let SaveIcon::Image(icon) = save.read_icon().unwrap() else {
            panic!("one frame should be a still image");
        };
        assert_eq!(icon.dimensions(), (48, 48));
        assert_eq!(icon.get_pixel(0, 0).0, [255, 255, 255, 255]);
    }

    fn write_utf16(target: &mut [u8], text: &str) {
        for (target, unit) in target.chunks_exact_mut(2).zip(text.encode_utf16()) {
            target.copy_from_slice(&unit.to_be_bytes());
        }
    }
}
