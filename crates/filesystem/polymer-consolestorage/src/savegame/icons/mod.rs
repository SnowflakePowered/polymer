use std::io::Cursor;

use image::{Delay, DynamicImage, Frame, ImageFormat, RgbaImage};
use polymer_banner::ps2::Ps2Icon;
use zenwebp::decoder::LoopCount;
use zenwebp::mux::{AnimationConfig, AnimationDecoder, AnimationEncoder};
use zenwebp::{EncoderConfig, PixelLayout};

use crate::Error;

mod dreamcast;
mod gamecube;
mod nintendo;
mod ps1;
pub(crate) mod ps2;
pub(crate) mod wii;
mod wiiu;

pub use nintendo::FromNintendoIcon;

pub enum SaveIcon {
    Image(RgbaImage),
    Animation(Vec<Frame>),
    Ps2Icon(Vec<u8>),
}

pub trait IconSave {
    fn read_icon(&self) -> Option<SaveIcon>;
}

impl SaveIcon {
    pub(crate) fn encode(&self) -> Result<Vec<u8>, Error> {
        match self {
            Self::Image(image) => {
                let mut bytes = Cursor::new(Vec::new());
                DynamicImage::ImageRgba8(image.clone())
                    .write_to(&mut bytes, ImageFormat::Png)
                    .map_err(|error| icon_error("encode PNG", error))?;
                Ok(bytes.into_inner())
            }
            Self::Animation(frames) => {
                let first = frames
                    .first()
                    .ok_or_else(|| icon_error("encode WebP", "animation has no frames"))?;
                let (width, height) = first.buffer().dimensions();
                let mut encoder = AnimationEncoder::new(
                    width,
                    height,
                    AnimationConfig {
                        loop_count: LoopCount::Forever,
                        ..AnimationConfig::default()
                    },
                )
                .map_err(|error| icon_error("encode WebP", error))?;
                let config = EncoderConfig::new_lossless();
                let mut timestamp = 0_u32;
                let mut last_duration = 1_u32;
                for frame in frames {
                    if frame.left() != 0
                        || frame.top() != 0
                        || frame.buffer().dimensions() != (width, height)
                    {
                        return Err(icon_error(
                            "encode WebP",
                            "animation frames must cover one common canvas",
                        ));
                    }
                    encoder
                        .add_frame(
                            frame.buffer().as_raw(),
                            PixelLayout::Rgba8,
                            timestamp,
                            &config,
                        )
                        .map_err(|error| icon_error("encode WebP", error))?;
                    let (numerator, denominator) = frame.delay().numer_denom_ms();
                    last_duration = numerator.div_ceil(denominator).max(1);
                    timestamp = timestamp.saturating_add(last_duration);
                }
                encoder
                    .finalize(last_duration)
                    .map_err(|error| icon_error("encode WebP", error))
            }
            Self::Ps2Icon(bytes) => Ok(bytes.clone()),
        }
    }

    pub fn data_url(&self) -> Result<String, Error> {
        use base64::Engine as _;
        let media = match self {
            Self::Image(_) => "image/png",
            Self::Animation(_) => "image/webp",
            Self::Ps2Icon(_) => Ps2Icon::MEDIA_TYPE,
        };
        Ok(format!(
            "data:{media};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(self.encode()?)
        ))
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            let mut decoder = AnimationDecoder::new(bytes)
                .map_err(|error| icon_error("decode animated WebP", error))?;
            let mut frames = Vec::with_capacity(decoder.info().frame_count as usize);
            while let Some(frame) = decoder
                .next_frame()
                .map_err(|error| icon_error("decode animated WebP", error))?
            {
                let pixels = frame.width as usize * frame.height as usize;
                let rgba = match frame.data.len() / pixels.max(1) {
                    3 => frame
                        .data
                        .chunks_exact(3)
                        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], u8::MAX])
                        .collect(),
                    _ => frame.data,
                };
                let image = RgbaImage::from_raw(frame.width, frame.height, rgba)
                    .ok_or_else(|| icon_error("decode animated WebP", "invalid frame layout"))?;
                frames.push(Frame::from_parts(
                    image,
                    0,
                    0,
                    Delay::from_numer_denom_ms(frame.duration_ms, 1),
                ));
            }
            return Ok(Self::Animation(frames));
        }
        if Ps2Icon::parse(bytes).is_some() {
            return Ok(Self::Ps2Icon(bytes.to_vec()));
        }
        image::load_from_memory_with_format(bytes, ImageFormat::Png)
            .map(|image| Self::Image(image.to_rgba8()))
            .map_err(|error| icon_error("decode PNG", error))
    }
}

fn icon_error(operation: &'static str, error: impl std::fmt::Display) -> Error {
    Error::HistoryConflict(format!("cannot {operation} save icon: {error}"))
}
