use image::{Delay, Frame, RgbaImage};

use super::{FromNintendoIcon, IconSave, SaveIcon};
use crate::impls::gamecube::gci::Save;

const ICON_WIDTH: u32 = 32;
const ICON_HEIGHT: u32 = 32;
const PALETTE_SIZE: usize = 256 * 2;
const CI8_SHARED_PALETTE: u8 = 1;
const RGB5A3: u8 = 2;
const CI8_UNIQUE_PALETTE: u8 = 3;

impl IconSave for Save<'_> {
    fn read_icon(&self) -> Option<SaveIcon> {
        let dirent = self.dirent();
        let formats = u16::from_be_bytes(dirent[0x30..0x32].try_into().ok()?);
        let speeds = u16::from_be_bytes(dirent[0x32..0x34].try_into().ok()?);
        if formats & 0b11 == 0 {
            return None;
        }

        let mut frames = Vec::new();
        let mut offsets = Vec::new();
        let mut data_length = 0_usize;
        let mut has_shared_palette = false;
        for frame in 0..8 {
            let speed = ((speeds >> (frame * 2)) & 0b11) as u8;
            if speed == 0 {
                break;
            }
            let format = ((formats >> (frame * 2)) & 0b11) as u8;
            frames.push((format, speed));
            offsets.push(data_length);
            data_length = data_length.checked_add(match format {
                CI8_SHARED_PALETTE => {
                    has_shared_palette = true;
                    ICON_WIDTH as usize * ICON_HEIGHT as usize
                }
                RGB5A3 => ICON_WIDTH as usize * ICON_HEIGHT as usize * 2,
                CI8_UNIQUE_PALETTE => ICON_WIDTH as usize * ICON_HEIGHT as usize + PALETTE_SIZE,
                _ => 0,
            })?;
        }
        if frames.is_empty() {
            return None;
        }

        let shared_palette_offset = data_length;
        if has_shared_palette {
            data_length = data_length.checked_add(PALETTE_SIZE)?;
        }
        let banner_length = match dirent[7] & 0b11 {
            1 => 96 * 32 + PALETTE_SIZE,
            2 => 96 * 32 * 2,
            _ => 0,
        };
        let image_offset = u32::from_be_bytes(dirent[0x2c..0x30].try_into().ok()?);
        if image_offset == u32::MAX {
            return None;
        }
        let offset = usize::try_from(image_offset)
            .ok()?
            .checked_add(banner_length)?;
        let data = self.data().get(offset..offset.checked_add(data_length)?)?;
        let shared_palette = has_shared_palette
            .then(|| data.get(shared_palette_offset..shared_palette_offset + PALETTE_SIZE))
            .flatten();

        let mut images = Vec::with_capacity(frames.len());
        for (frame, &(_, speed)) in frames.iter().enumerate() {
            // The IPL forwards empty-format slots to the next decodable frame.
            let image = (frame..frames.len())
                .find_map(|candidate| {
                    decode_frame(
                        data,
                        offsets[candidate],
                        frames[candidate].0,
                        shared_palette,
                    )
                })
                .unwrap_or_else(|| RgbaImage::new(ICON_WIDTH, ICON_HEIGHT));
            images.push((image, speed));
        }

        if images.len() == 1 {
            return Some(SaveIcon::Image(images.pop()?.0));
        }
        if dirent[7] & 0b100 != 0 && images.len() > 2 {
            let reverse = images[1..images.len() - 1]
                .iter()
                .rev()
                .cloned()
                .collect::<Vec<_>>();
            images.extend(reverse);
        }
        Some(SaveIcon::Animation(
            images
                .into_iter()
                .map(|(image, speed)| {
                    let delay = Delay::from_numer_denom_ms(u32::from(speed) * 4_000, 60);
                    Frame::from_parts(image, 0, 0, delay)
                })
                .collect(),
        ))
    }
}

fn decode_frame(
    data: &[u8],
    offset: usize,
    format: u8,
    shared_palette: Option<&[u8]>,
) -> Option<RgbaImage> {
    let data = data.get(offset..)?;
    match format {
        CI8_SHARED_PALETTE => RgbaImage::from_ci8(
            data.get(..ICON_WIDTH as usize * ICON_HEIGHT as usize)?,
            shared_palette?,
            ICON_WIDTH,
            ICON_HEIGHT,
        ),
        RGB5A3 => RgbaImage::from_rgb5a3(data, ICON_WIDTH, ICON_HEIGHT),
        CI8_UNIQUE_PALETTE => {
            let pixels = ICON_WIDTH as usize * ICON_HEIGHT as usize;
            RgbaImage::from_ci8(
                data.get(..pixels)?,
                data.get(pixels..pixels + PALETTE_SIZE)?,
                ICON_WIDTH,
                ICON_HEIGHT,
            )
        }
        _ => None,
    }
}
