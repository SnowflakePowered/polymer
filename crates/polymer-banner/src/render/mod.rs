mod ctr;
mod mesh;
mod renderer;
mod webp;
mod wii;

use crate::Banner;
use dashmap::DashMap;
use peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
use std::sync::{Arc, LazyLock};

pub use renderer::BannerTextures;

static IMAGES: LazyLock<DashMap<u64, Banner>> = LazyLock::new(DashMap::new);

/// Placeholder images that the shell rasterizer replaces with a rendered
/// banner. The placeholder pixels are transparent, so a rasterizer without
/// banner support draws nothing instead of failing.
pub struct BannerImages;

impl BannerImages {
    pub fn placeholder(banner: Banner, width: u32, height: u32) -> ImageData {
        let image = ImageData {
            data: Blob::new(Arc::new(vec![0; width as usize * height as usize * 4])),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::AlphaPremultiplied,
            width,
            height,
        };
        IMAGES.insert(image.data.id(), banner);
        image
    }

    pub fn release(image: &ImageData) {
        IMAGES.remove(&image.data.id());
    }

    fn banner(image: &ImageData) -> Option<Banner> {
        IMAGES.get(&image.data.id()).map(|banner| banner.clone())
    }

    fn contains(id: u64) -> bool {
        IMAGES.contains_key(&id)
    }
}
