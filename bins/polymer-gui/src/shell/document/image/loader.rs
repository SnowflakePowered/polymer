use blitz_traits::net::{Bytes, NetHandler, NetProvider, NetWaker, Request, Url};
use dashmap::DashMap;
use parking_lot::Mutex;
use peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};
use polymer_mipmap::{MipImages, MipSource};
use std::sync::{
    Arc, OnceLock, Weak,
    atomic::{AtomicU32, Ordering},
};

const MAX_DIMENSION: u32 = 8192;

pub(in crate::shell::document) struct ImageLoader {
    net: Arc<dyn NetProvider>,
    waker: Arc<dyn NetWaker>,
    doc_id: usize,
    base: Option<Url>,
    scale_factor: AtomicU32,
    entries: DashMap<String, Arc<ImageEntry>>,
}

impl ImageLoader {
    pub(in crate::shell::document) fn new(
        net: Arc<dyn NetProvider>,
        waker: Arc<dyn NetWaker>,
        doc_id: usize,
        base_url: &str,
    ) -> Self {
        Self {
            net,
            waker,
            doc_id,
            base: Url::parse(base_url).ok(),
            scale_factor: AtomicU32::new(1.0f32.to_bits()),
            entries: DashMap::new(),
        }
    }

    pub(in crate::shell::document) fn set_scale_factor(&self, scale_factor: f32) {
        self.scale_factor
            .store(scale_factor.to_bits(), Ordering::Relaxed);
    }

    pub(super) fn scale_factor(&self) -> f64 {
        f64::from(f32::from_bits(self.scale_factor.load(Ordering::Relaxed)))
    }

    pub(super) fn fetch(&self, source: &str) -> Option<Arc<ImageEntry>> {
        let url = Url::options()
            .base_url(self.base.as_ref())
            .parse(source)
            .ok()?;
        if let Some(entry) = self.entries.get(url.as_str()) {
            return Some(entry.clone());
        }
        let entry = Arc::new(ImageEntry {
            url,
            size: OnceLock::new(),
            source: MipSource::new(),
            state: Mutex::new(EntryState {
                loading: true,
                generation: MipImages::generation(),
                ..EntryState::default()
            }),
        });
        self.entries.insert(entry.url.to_string(), entry.clone());
        self.load(&entry);
        Some(entry)
    }

    fn load(&self, entry: &Arc<ImageEntry>) {
        self.net.fetch(
            self.doc_id,
            Request::get(entry.url.clone()),
            Box::new(DecodeHandler {
                entry: Arc::downgrade(entry),
                waker: self.waker.clone(),
                doc_id: self.doc_id,
                delivered: false,
            }),
        );
    }
}

#[derive(Default)]
struct EntryState {
    decoded: Option<ImageData>,
    level: Option<u32>,
    revision: u64,
    generation: u64,
    loading: bool,
    failed: Option<u64>,
}

/// One image URL. Its decoded pixels are only held until they are handed to
/// the rasterizer, which keeps the mip chain from `level` down; showing it
/// larger than that level loads it again.
pub(super) struct ImageEntry {
    url: Url,
    size: OnceLock<(u32, u32)>,
    source: Arc<MipSource>,
    state: Mutex<EntryState>,
}

impl ImageEntry {
    pub(super) fn size(&self) -> Option<(u32, u32)> {
        self.size.get().copied()
    }

    pub(super) fn source(&self) -> &Arc<MipSource> {
        &self.source
    }

    /// Ensures a mip chain from `level` down is, or will be, uploaded. When
    /// the image can be drawn now, returns the revision of its upload.
    pub(super) fn prepare(self: &Arc<Self>, loader: &ImageLoader, level: u32) -> Option<u64> {
        let mut state = self.state.lock();
        let generation = MipImages::generation();
        if state.generation != generation {
            state.generation = generation;
            state.level = None;
        }
        if let Some(pixels) = state.decoded.take() {
            let level = state.level.map_or(level, |kept| kept.min(level));
            self.source.upload(pixels, level);
            state.level = Some(level);
            state.revision += 1;
            return Some(state.revision);
        }
        match state.level {
            Some(kept) if kept <= level => return Some(state.revision),
            Some(_) if self.source.lower_pending(level) => {
                state.level = Some(level);
                return Some(state.revision);
            }
            _ => {}
        }
        let drawable = state.level.map(|_| state.revision);
        let reload = !state.loading && state.failed != Some(generation);
        state.loading |= reload;
        drop(state);
        if reload {
            loader.load(self);
        }
        drawable
    }

    fn deliver(&self, pixels: ImageData) {
        let _ = self.size.set((pixels.width, pixels.height));
        let mut state = self.state.lock();
        state.decoded = Some(pixels);
        state.loading = false;
        state.failed = None;
    }

    fn fail(&self) {
        let mut state = self.state.lock();
        state.loading = false;
        state.failed = Some(state.generation);
    }
}

struct DecodeHandler {
    entry: Weak<ImageEntry>,
    waker: Arc<dyn NetWaker>,
    doc_id: usize,
    delivered: bool,
}

impl Drop for DecodeHandler {
    fn drop(&mut self) {
        if !self.delivered
            && let Some(entry) = self.entry.upgrade()
        {
            entry.fail();
        }
    }
}

impl NetHandler for DecodeHandler {
    fn bytes(mut self: Box<Self>, _resolved_url: String, bytes: Bytes) {
        rayon::spawn(move || {
            if self.entry.strong_count() == 0 {
                return;
            }
            let Ok(mut image) = image::load_from_memory(&bytes) else {
                return;
            };
            if image.width().max(image.height()) > MAX_DIMENSION {
                image = image.resize(
                    MAX_DIMENSION,
                    MAX_DIMENSION,
                    image::imageops::FilterType::Triangle,
                );
            }
            let image = image.into_rgba8();
            let (width, height) = image.dimensions();
            if let Some(entry) = self.entry.upgrade() {
                entry.deliver(ImageData {
                    data: Blob::new(Arc::new(image.into_raw())),
                    format: ImageFormat::Rgba8,
                    alpha_type: ImageAlphaType::Alpha,
                    width,
                    height,
                });
                self.delivered = true;
                self.waker.wake(self.doc_id);
            }
        });
    }
}
