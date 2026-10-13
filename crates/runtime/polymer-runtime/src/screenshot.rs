use image::{DynamicImage, ImageFormat, RgbaImage};
use std::io::Cursor;

#[derive(Clone, Debug)]
pub struct RgbaScreenshot {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl RgbaScreenshot {
    pub fn encode(self, format: ImageFormat) -> Result<Vec<u8>, image::ImageError> {
        let image = RgbaImage::from_raw(self.width, self.height, self.pixels)
            .filter(|image| image.width() != 0 && image.height() != 0)
            .ok_or_else(|| {
                image::ImageError::Parameter(image::error::ParameterError::from_kind(
                    image::error::ParameterErrorKind::DimensionMismatch,
                ))
            })?;
        let mut bytes = Cursor::new(Vec::new());
        match format {
            ImageFormat::Jpeg => DynamicImage::ImageRgba8(image)
                .into_rgb8()
                .write_to(&mut bytes, format)?,
            _ => image.write_to(&mut bytes, format)?,
        }
        Ok(bytes.into_inner())
    }
}
