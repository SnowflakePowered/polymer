//! 3DS HOME Menu banners: the CGFX scenes inside an ExeFS `banner` CBMD.
use cgfx_parser::GraphicsFile;
use cgfx_renderer::Scene;
use zerocopy::little_endian::U32;
use zerocopy::{FromBytes, Immutable, KnownLayout};

#[derive(FromBytes, KnownLayout, Immutable)]
#[repr(C)]
struct CbmdHeader {
    magic: [u8; 4],
    _zero: U32,
    /// The common scene, then one locale overlay per HOME Menu region and language.
    scenes: [U32; 14],
    _reserved: [u8; 0x44],
    cwav: U32,
}

const _: () = assert!(size_of::<CbmdHeader>() == 0x88);

/// A banner scene with its common and locale layers merged.
pub struct CtrBanner {
    pub scene: Scene,
}

impl CtrBanner {
    /// A CBMD banner without its trailing CWAV sound.
    pub const MEDIA_TYPE: &str = "application/x-3ds-banner";
    const MAX_SCENE_SIZE: usize = 16 * 1024 * 1024;
    /// USA English, then EUR English.
    const LOCALES: [usize; 2] = [10, 1];

    pub fn parse(cbmd: &[u8]) -> Option<Self> {
        let (header, _) = CbmdHeader::ref_from_prefix(cbmd).ok()?;
        if header.magic != *b"CBMD" {
            return None;
        }
        let layer = |slot: usize| {
            let offset = usize::try_from(header.scenes[slot].get()).ok()?;
            let data = cbmd.get(offset..).filter(|_| offset != 0)?;
            let cgfx = match data.starts_with(b"CGFX") {
                true => data.to_vec(),
                false => Self::decompress_lz11(data)?,
            };
            match cgfx_parser::parse(&cgfx).ok()? {
                GraphicsFile::Cgfx(cgfx) => Scene::from_cgfx(&cgfx).ok(),
                GraphicsFile::Bch(bch) => Scene::from_bch(&bch).ok(),
            }
        };
        let mut scene = layer(0)?;
        if let Some(locale) = Self::LOCALES.into_iter().find_map(layer) {
            scene.merge_override(locale);
        }
        (!scene.models.is_empty()).then_some(Self { scene })
    }

    /// The CBMD of an ExeFS `banner`, ready to store under [`Self::MEDIA_TYPE`].
    pub fn strip_sound(banner: &[u8]) -> Option<&[u8]> {
        let (header, _) = CbmdHeader::ref_from_prefix(banner).ok()?;
        if header.magic != *b"CBMD" {
            return None;
        }
        match usize::try_from(header.cwav.get()).ok()? {
            0 => Some(banner),
            end => banner.get(..end),
        }
    }

    /// Nintendo LZ11: a type byte, a little-endian 24-bit size, then flag-led
    /// runs of literals and back-references with 4, 8 or 16-bit lengths.
    fn decompress_lz11(bytes: &[u8]) -> Option<Vec<u8>> {
        if bytes.first() != Some(&0x11) {
            return None;
        }
        let size = u32::from_le_bytes([*bytes.get(1)?, *bytes.get(2)?, *bytes.get(3)?, 0]);
        let size = usize::try_from(size)
            .ok()
            .filter(|size| *size <= Self::MAX_SCENE_SIZE)?;
        let mut input = bytes.get(4..)?.iter().copied();
        let mut output = Vec::with_capacity(size);
        while output.len() < size {
            let flags = input.next()?;
            for bit in (0..8).rev() {
                if output.len() == size {
                    break;
                }
                if flags & (1 << bit) == 0 {
                    output.push(input.next()?);
                    continue;
                }
                let first = input.next()?;
                let (length, high) = match first >> 4 {
                    0 => {
                        let next = input.next()?;
                        (
                            (usize::from(first & 0x0f) << 4 | usize::from(next >> 4)) + 0x11,
                            next,
                        )
                    }
                    1 => {
                        let [middle, next] = [input.next()?, input.next()?];
                        let length = usize::from(first & 0x0f) << 12
                            | usize::from(middle) << 4
                            | usize::from(next >> 4);
                        (length + 0x111, next)
                    }
                    length => (usize::from(length) + 1, first),
                };
                let distance = (usize::from(high & 0x0f) << 8 | usize::from(input.next()?)) + 1;
                if distance > output.len() || output.len() + length > size {
                    return None;
                }
                for _ in 0..length {
                    output.push(output[output.len() - distance]);
                }
            }
        }
        Some(output)
    }
}
