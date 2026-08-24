use image::{Delay, Frame, RgbaImage};

use super::{IconSave, SaveIcon};
use crate::impls::ps1::IconSaveContainer;

impl IconSave for IconSaveContainer<'_> {
    fn read_icon(&self) -> Option<SaveIcon> {
        const FRAME_SIZE: usize = 128;
        const PALETTE_OFFSET: usize = 96;

        let block = self.blocks().next()?;
        if block.get(..2)? != b"SC" {
            return None;
        }
        let icon_count = match usize::from(block[2] & 0x03) {
            count @ 1..=3 => count,
            _ => return None,
        };
        let palette = block.get(PALETTE_OFFSET..FRAME_SIZE)?;
        let frames = block.get(FRAME_SIZE..(icon_count + 1).checked_mul(FRAME_SIZE)?)?;
        let images = frames
            .chunks_exact(FRAME_SIZE)
            .filter_map(|frame| {
                let mut pixels = Vec::with_capacity(16 * 16 * 4);
                for &byte in frame {
                    for shift in [0, 4] {
                        let offset = usize::from((byte >> shift) & 0x0f) * 2;
                        let color = u16::from_le_bytes([palette[offset], palette[offset + 1]]);
                        pixels.extend_from_slice(&[
                            ((color & 0x1f) * 255 / 31) as u8,
                            (((color >> 5) & 0x1f) * 255 / 31) as u8,
                            (((color >> 10) & 0x1f) * 255 / 31) as u8,
                            if color == 0 { 0 } else { 255 },
                        ]);
                    }
                }
                RgbaImage::from_raw(16, 16, pixels)
            })
            .collect::<Vec<_>>();
        match images.as_slice() {
            [] => None,
            [image] => Some(SaveIcon::Image(image.clone())),
            _ => Some(SaveIcon::Animation(
                images
                    .into_iter()
                    .map(|image| {
                        Frame::from_parts(image, 0, 0, Delay::from_numer_denom_ms(1000, 10))
                    })
                    .collect(),
            )),
        }
    }
}
