use std::borrow::Cow;
use std::io::Cursor;

use gltf::binary::{Glb, Header};
use image::{Delay, DynamicImage, Frame, ImageFormat, RgbaImage};
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
    Model(gltf::Gltf),
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
            Self::Model(model) => {
                let json = serde_json::to_vec(model.document.as_json())
                    .map_err(|error| icon_error("encode GLB", error))?;
                Glb {
                    header: Header {
                        magic: *b"glTF",
                        version: 2,
                        length: 0,
                    },
                    json: Cow::Owned(json),
                    bin: model.blob.clone().map(Cow::Owned),
                }
                .to_vec()
                .map_err(|error| icon_error("encode GLB", error))
            }
        }
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.starts_with(b"glTF") {
            return gltf::Gltf::from_slice(bytes)
                .map(Self::Model)
                .map_err(|error| icon_error("decode GLB", error));
        }
        if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            let mut decoder = AnimationDecoder::new(bytes)
                .map_err(|error| icon_error("decode animated WebP", error))?;
            let mut frames = Vec::with_capacity(decoder.info().frame_count as usize);
            while let Some(frame) = decoder
                .next_frame()
                .map_err(|error| icon_error("decode animated WebP", error))?
            {
                let image = RgbaImage::from_raw(frame.width, frame.height, frame.data)
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
        image::load_from_memory_with_format(bytes, ImageFormat::Png)
            .map(|image| Self::Image(image.to_rgba8()))
            .map_err(|error| icon_error("decode PNG", error))
    }
}

fn icon_error(operation: &'static str, error: impl std::fmt::Display) -> Error {
    Error::HistoryConflict(format!("cannot {operation} save icon: {error}"))
}
