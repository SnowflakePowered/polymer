use image::{Delay, Frame, Rgba, RgbaImage};

use super::{IconSave, SaveIcon};
use crate::impls::dreamcast::vms::Save;

const ICON_SIZE: usize = 512;

impl IconSave for Save<'_> {
    fn read_icon(&self) -> Option<SaveIcon> {
        let header = self.header().ok()?;
        let mut images = header.icons().chunks_exact(ICON_SIZE).map(|icon| {
            let mut image = RgbaImage::new(32, 32);
            for (index, pixel) in image.pixels_mut().enumerate() {
                let byte = icon[index / 2];
                let palette_index = if index % 2 == 0 {
                    byte >> 4
                } else {
                    byte & 0x0f
                };
                let color = header.icon_color(palette_index);
                let expand = |value: u16| ((value << 4) | value) as u8;
                *pixel = Rgba([
                    expand((color >> 8) & 0x0f),
                    expand((color >> 4) & 0x0f),
                    expand(color & 0x0f),
                    expand((color >> 12) & 0x0f),
                ]);
            }
            image
        });
        let first = images.next()?;
        let remaining = images.collect::<Vec<_>>();
        if remaining.is_empty() {
            return Some(SaveIcon::Image(first));
        }

        let speed = u32::from(header.animation_speed()).max(1);
        let delay = Delay::from_numer_denom_ms(speed * 1_000, 30);
        Some(SaveIcon::Animation(
            std::iter::once(first)
                .chain(remaining)
                .map(|image| Frame::from_parts(image, 0, 0, delay))
                .collect(),
        ))
    }
}
