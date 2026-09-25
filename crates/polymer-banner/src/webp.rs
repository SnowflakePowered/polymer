//! Animated WebP save icons, played back by swapping frames on the GPU.
use zenwebp::mux::AnimationDecoder;

/// Every composited frame of an animated WebP, as straight-alpha RGBA.
pub struct AnimatedWebp {
    pub width: u32,
    pub height: u32,
    pub frames: Vec<WebpFrame>,
}

pub struct WebpFrame {
    pub rgba: Vec<u8>,
    pub duration_ms: u32,
}

impl AnimatedWebp {
    pub const MEDIA_TYPE: &str = "image/webp";

    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let mut decoder = AnimationDecoder::new(bytes).ok()?;
        let info = decoder.info();
        let pixels = info.canvas_width as usize * info.canvas_height as usize;
        let mut frames = Vec::with_capacity(info.frame_count as usize);
        while let Some(frame) = decoder.next_frame().ok()? {
            let rgba = match frame.data.len() / pixels {
                4 => frame.data,
                3 => frame
                    .data
                    .chunks_exact(3)
                    .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], u8::MAX])
                    .collect(),
                _ => return None,
            };
            frames.push(WebpFrame {
                rgba,
                duration_ms: frame.duration_ms.max(1),
            });
        }
        (!frames.is_empty()).then_some(Self {
            width: info.canvas_width,
            height: info.canvas_height,
            frames,
        })
    }
}
