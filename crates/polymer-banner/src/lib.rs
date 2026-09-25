//! Console banner and save icon formats, and the renderers that draw the ones
//! a browser cannot display natively.
pub mod ctr;
pub mod ps2;
#[cfg(feature = "render")]
mod render;
pub mod webp;
pub mod wii;

#[cfg(feature = "render")]
pub use render::{BannerImages, BannerTextures};

use std::sync::Arc;

/// A parsed banner the shell renders natively, chosen by its media type.
#[derive(Clone)]
pub enum Banner {
    Ps2(Arc<ps2::Ps2Icon>),
    Wii(Arc<wii::WiiBanner>),
    Webp(Arc<webp::AnimatedWebp>),
    Ctr(Arc<ctr::CtrBanner>),
}

impl Banner {
    pub fn parse(media_type: &str, bytes: &[u8]) -> Option<Self> {
        match media_type {
            ps2::Ps2Icon::MEDIA_TYPE => {
                ps2::Ps2Icon::parse(bytes).map(|icon| Self::Ps2(Arc::new(icon)))
            }
            wii::WiiBanner::MEDIA_TYPE => {
                wii::WiiBanner::parse(bytes).map(|banner| Self::Wii(Arc::new(banner)))
            }
            webp::AnimatedWebp::MEDIA_TYPE => {
                webp::AnimatedWebp::parse(bytes).map(|image| Self::Webp(Arc::new(image)))
            }
            ctr::CtrBanner::MEDIA_TYPE => {
                ctr::CtrBanner::parse(bytes).map(|banner| Self::Ctr(Arc::new(banner)))
            }
            _ => None,
        }
    }

    /// The pixel size of banners that are scaled by the compositor rather
    /// than rendered at the element's size.
    pub fn native_size(&self) -> Option<(u32, u32)> {
        match self {
            Self::Webp(image) => Some((image.width, image.height)),
            Self::Ps2(_) | Self::Wii(_) | Self::Ctr(_) => None,
        }
    }
}
