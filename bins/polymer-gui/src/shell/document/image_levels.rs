use kurbo::Affine;
use peniko::{Blob, ImageAlphaType, ImageData, WeakBlob};
use std::{
    collections::{HashMap, hash_map::Entry},
    sync::{
        Arc,
        mpsc::{Receiver, Sender, channel},
    },
};

const BUDGET_BYTES: usize = 64 << 20;
const MAX_LEVEL: u32 = 12;

type LevelKey = (u64, u32);

struct Level {
    source: WeakBlob<u8>,
    image: ImageData,
    last_used: u64,
}

struct LevelJob {
    key: LevelKey,
    image: ImageData,
}

impl LevelJob {
    fn reduce(&self) -> ImageData {
        let factor = 1usize << self.key.1;
        let (width, height) = (self.image.width as usize, self.image.height as usize);
        let (reduced_width, reduced_height) = (width.div_ceil(factor), height.div_ceil(factor));
        let source = self.image.data.data();
        let straight = self.image.alpha_type == ImageAlphaType::Alpha;
        let mut sums = vec![[0u64; 4]; reduced_width];
        let mut pixels = Vec::with_capacity(reduced_width * reduced_height * 4);
        for reduced_y in 0..reduced_height {
            sums.fill([0; 4]);
            let rows = reduced_y * factor..((reduced_y + 1) * factor).min(height);
            let row_count = rows.len();
            for row in rows.map(|y| &source[y * width * 4..(y + 1) * width * 4]) {
                for (x, pixel) in row.chunks_exact(4).enumerate() {
                    let alpha = u64::from(pixel[3]);
                    let weight = if straight { alpha } else { 255 };
                    let sum = &mut sums[x / factor];
                    sum[0] += u64::from(pixel[0]) * weight;
                    sum[1] += u64::from(pixel[1]) * weight;
                    sum[2] += u64::from(pixel[2]) * weight;
                    sum[3] += alpha * 255;
                }
            }
            for (reduced_x, sum) in sums.iter().enumerate() {
                let columns = ((reduced_x + 1) * factor).min(width) - reduced_x * factor;
                let divisor = (row_count * columns) as u64 * 255;
                pixels.extend(sum.map(|channel| ((channel + divisor / 2) / divisor) as u8));
            }
        }
        ImageData {
            data: Blob::new(Arc::new(pixels)),
            format: self.image.format,
            alpha_type: ImageAlphaType::AlphaPremultiplied,
            width: reduced_width as u32,
            height: reduced_height as u32,
        }
    }
}

pub(super) struct ImageLevels {
    levels: HashMap<LevelKey, Level>,
    pending: HashMap<LevelKey, WeakBlob<u8>>,
    jobs: Sender<LevelJob>,
    finished: Receiver<(LevelKey, ImageData)>,
    bytes: usize,
    frame: u64,
    scale_factor: f64,
}

impl ImageLevels {
    pub(super) fn new(wake: impl Fn() + Send + 'static) -> Self {
        let (jobs, job_queue) = channel::<LevelJob>();
        let (results, finished) = channel();
        std::thread::Builder::new()
            .name("polymer-image-levels".into())
            .spawn(move || {
                for job in job_queue {
                    let reduced = job.reduce();
                    if results.send((job.key, reduced)).is_err() {
                        break;
                    }
                    wake();
                }
            })
            .expect("spawning the image level worker");
        Self {
            levels: HashMap::new(),
            pending: HashMap::new(),
            jobs,
            finished,
            bytes: 0,
            frame: 0,
            scale_factor: 1.0,
        }
    }

    pub(super) fn begin_frame(&mut self, scale_factor: f64) -> bool {
        self.frame += 1;
        self.scale_factor = scale_factor;
        let mut arrived = false;
        while let Ok((key, image)) = self.finished.try_recv() {
            let Some(source) = self.pending.remove(&key) else {
                continue;
            };
            self.bytes += image.data.len();
            self.levels.insert(
                key,
                Level {
                    source,
                    image,
                    last_used: self.frame,
                },
            );
            arrived = true;
        }
        self.levels.retain(|_, level| {
            let alive = level.source.upgrade().is_some();
            if !alive {
                self.bytes -= level.image.data.len();
            }
            alive
        });
        while self.bytes > BUDGET_BYTES {
            let Some(key) = self
                .levels
                .iter()
                .filter(|(_, level)| level.last_used < self.frame)
                .min_by_key(|(_, level)| level.last_used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            let level = self.levels.remove(&key).expect("evicted key exists");
            self.bytes -= level.image.data.len();
        }
        arrived
    }

    pub(super) fn reduce(
        &mut self,
        image: &ImageData,
        transform: Affine,
    ) -> Option<(ImageData, Affine)> {
        let [a, b, c, d, _, _] = transform.as_coeffs();
        let scale = a.hypot(b).max(c.hypot(d)) * self.scale_factor;
        if !(scale > 0.0 && scale <= 0.5) {
            return None;
        }
        let size_limit = image.width.max(image.height).ilog2();
        let level = ((1.0 / scale).log2().floor() as u32)
            .min(MAX_LEVEL)
            .min(size_limit);
        if level == 0 {
            return None;
        }
        let key = (image.data.id(), level);
        if let Some(entry) = self.levels.get_mut(&key) {
            entry.last_used = self.frame;
            let stretch = Affine::scale_non_uniform(
                f64::from(image.width) / f64::from(entry.image.width),
                f64::from(image.height) / f64::from(entry.image.height),
            );
            return Some((entry.image.clone(), stretch));
        }
        if let Entry::Vacant(pending) = self.pending.entry(key) {
            pending.insert(image.data.downgrade());
            let _ = self.jobs.send(LevelJob {
                key,
                image: image.clone(),
            });
        }
        None
    }
}
