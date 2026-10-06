const COMPONENTS_X: u32 = 4;
const COMPONENTS_Y: u32 = 3;
const SAMPLE_SIZE: u32 = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaPreview {
    pub width: u32,
    pub height: u32,
    pub blurhash: String,
}

impl MediaPreview {
    pub fn from_image(bytes: &[u8]) -> Option<Self> {
        let image = image::load_from_memory(bytes).ok()?;
        let sample = image.thumbnail_exact(SAMPLE_SIZE, SAMPLE_SIZE).to_rgba8();
        Some(Self {
            width: image.width(),
            height: image.height(),
            blurhash: blurhash::encode(
                COMPONENTS_X,
                COMPONENTS_Y,
                SAMPLE_SIZE,
                SAMPLE_SIZE,
                sample.as_raw(),
            )
            .ok()?,
        })
    }
}
