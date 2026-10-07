//! Decoded images that the shell rasterizer keeps only as GPU mip chains,
//! drawn through placeholders it resolves at their destination size.
#[cfg(feature = "render")]
mod render;

#[cfg(feature = "render")]
pub use render::MipTextures;

use dashmap::DashMap;
use parking_lot::Mutex;
use peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
use std::sync::{
    Arc, LazyLock, Weak,
    atomic::{AtomicU64, Ordering},
};

static SOURCES: LazyLock<DashMap<u64, Weak<MipSource>>> = LazyLock::new(DashMap::new);
static PLACEHOLDERS: LazyLock<DashMap<u64, Placement>> = LazyLock::new(DashMap::new);
static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);
static GENERATION: AtomicU64 = AtomicU64::new(0);

struct Placement {
    source: Arc<MipSource>,
    nearest: bool,
}

struct Upload {
    pixels: ImageData,
    level: u32,
}

/// One decoded image. Its straight-alpha RGBA8 pixels wait here only until
/// the rasterizer takes them to build the mip chain from `level` down.
pub struct MipSource {
    id: u64,
    upload: Mutex<Option<Upload>>,
}

impl MipSource {
    pub fn new() -> Arc<Self> {
        let source = Arc::new(Self {
            id: NEXT_SOURCE.fetch_add(1, Ordering::Relaxed),
            upload: Mutex::new(None),
        });
        SOURCES.insert(source.id, Arc::downgrade(&source));
        source
    }

    pub fn upload(&self, pixels: ImageData, level: u32) {
        *self.upload.lock() = Some(Upload { pixels, level });
    }

    /// Lowers the first kept level of an upload the rasterizer has not taken
    /// yet, reporting whether there was one.
    pub fn lower_pending(&self, level: u32) -> bool {
        let mut upload = self.upload.lock();
        let Some(upload) = upload.as_mut() else {
            return false;
        };
        upload.level = upload.level.min(level);
        true
    }

    /// A placeholder of `width` x `height` device pixels that the rasterizer
    /// replaces with this image resampled from its mip chain.
    pub fn placeholder(self: &Arc<Self>, width: u32, height: u32, nearest: bool) -> MipPlaceholder {
        let image = ImageData {
            data: Blob::new(Arc::new([])),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::AlphaPremultiplied,
            width,
            height,
        };
        PLACEHOLDERS.insert(
            image.data.id(),
            Placement {
                source: self.clone(),
                nearest,
            },
        );
        MipPlaceholder { image }
    }

    fn take(&self) -> Option<Upload> {
        self.upload.lock().take()
    }
}

impl Drop for MipSource {
    fn drop(&mut self) {
        SOURCES.remove(&self.id);
    }
}

pub struct MipPlaceholder {
    image: ImageData,
}

impl MipPlaceholder {
    pub fn image(&self) -> &ImageData {
        &self.image
    }
}

impl Drop for MipPlaceholder {
    fn drop(&mut self) {
        PLACEHOLDERS.remove(&self.image.data.id());
    }
}

pub struct MipImages;

impl MipImages {
    /// Drops every pending upload and starts a new generation. The rasterizer
    /// evicts all mip chains when it receives a frame of the new generation.
    pub fn clear() {
        let sources = SOURCES
            .iter()
            .filter_map(|source| source.upgrade())
            .collect::<Vec<_>>();
        for source in sources {
            source.take();
        }
        GENERATION.fetch_add(1, Ordering::Relaxed);
    }

    pub fn generation() -> u64 {
        GENERATION.load(Ordering::Relaxed)
    }
}
