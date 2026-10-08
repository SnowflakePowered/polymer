use image::{Rgba, RgbaImage};

pub trait FromNintendoIcon: Sized {
    fn from_rgb5a3(data: &[u8], width: u32, height: u32) -> Option<Self>;

    fn from_ci8(data: &[u8], palette: &[u8], width: u32, height: u32) -> Option<Self>;
}

impl FromNintendoIcon for RgbaImage {
    fn from_rgb5a3(data: &[u8], width: u32, height: u32) -> Option<Self> {
        if !width.is_multiple_of(4) || !height.is_multiple_of(4) {
            return None;
        }
        let data = data.get(..pixel_count(width, height)?.checked_mul(2)?)?;
        let mut image = Self::new(width, height);
        let mut source = 0;
        for block_y in (0..height).step_by(4) {
            for block_x in (0..width).step_by(4) {
                for y in 0..4 {
                    for x in 0..4 {
                        let value = u16::from_be_bytes([data[source], data[source + 1]]);
                        source += 2;
                        image.put_pixel(block_x + x, block_y + y, decode_rgb5a3_color(value));
                    }
                }
            }
        }
        Some(image)
    }

    fn from_ci8(data: &[u8], palette: &[u8], width: u32, height: u32) -> Option<Self> {
        if !width.is_multiple_of(8) || !height.is_multiple_of(4) {
            return None;
        }
        let data = data.get(..pixel_count(width, height)?)?;
        let palette = palette.get(..256 * 2)?;
        let mut image = Self::new(width, height);
        let mut source = 0;
        for block_y in (0..height).step_by(4) {
            for block_x in (0..width).step_by(8) {
                for y in 0..4 {
                    for x in 0..8 {
                        let palette_offset = usize::from(data[source]) * 2;
                        source += 1;
                        let value = u16::from_be_bytes([
                            palette[palette_offset],
                            palette[palette_offset + 1],
                        ]);
                        image.put_pixel(block_x + x, block_y + y, decode_rgb5a3_color(value));
                    }
                }
            }
        }
        Some(image)
    }
}

fn pixel_count(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width.checked_mul(height)?).ok()
}

fn decode_rgb5a3_color(value: u16) -> Rgba<u8> {
    if value & 0x8000 != 0 {
        let expand = |channel: u16| ((channel << 3) | (channel >> 2)) as u8;
        Rgba([
            expand((value >> 10) & 0x1f),
            expand((value >> 5) & 0x1f),
            expand(value & 0x1f),
            255,
        ])
    } else {
        let expand = |channel: u16| ((channel << 4) | channel) as u8;
        Rgba([
            expand((value >> 8) & 0x0f),
            expand((value >> 4) & 0x0f),
            expand(value & 0x0f),
            (((value >> 12) & 0x07) * 255 / 7) as u8,
        ])
    }
}
