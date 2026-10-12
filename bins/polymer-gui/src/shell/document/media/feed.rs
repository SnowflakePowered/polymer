use parking_lot::Mutex;
use polymer_composition::VideoAudio;
use rodio::{ChannelCount, SampleRate, Source, buffer::SamplesBuffer, mixer::Mixer};
use std::{collections::VecDeque, num::NonZero, sync::Arc, time::Duration};

const BLOCK_FRAMES: usize = 256;

type Format = (ChannelCount, SampleRate);

#[derive(Default)]
struct FeedState {
    mixer: Option<Mixer>,
    format: Option<Format>,
    sources: VecDeque<Box<dyn Source + Send>>,
    gain: f32,
    paused: bool,
    closed: bool,
    attached: bool,
    epoch: u64,
    played: u64,
}

impl FeedState {
    fn pull(&mut self) -> Option<f32> {
        while let Some(source) = self.sources.front_mut() {
            if let Some(sample) = source.next() {
                self.played += 1;
                return Some(sample);
            }
            self.sources.pop_front();
        }
        None
    }

    fn idle(&self) -> bool {
        self.paused || self.sources.is_empty()
    }
}

#[derive(Clone)]
pub(super) struct AudioFeed(Arc<Mutex<FeedState>>);

impl AudioFeed {
    pub fn new(mixer: Option<Mixer>, gain: f32) -> Self {
        Self(Arc::new(Mutex::new(FeedState {
            mixer,
            gain,
            ..FeedState::default()
        })))
    }

    fn attach(&self, state: &mut FeedState) {
        state.epoch += 1;
        state.attached = false;
        let (Some(mixer), Some(format)) = (&state.mixer, state.format) else {
            return;
        };
        if state.closed || state.idle() {
            return;
        }
        mixer.add(FeedSource {
            feed: self.clone(),
            epoch: state.epoch,
            format,
            block: Vec::with_capacity(BLOCK_FRAMES * usize::from(format.0.get())),
            cursor: 0,
        });
        state.attached = true;
    }

    fn wake(&self, state: &mut FeedState) {
        if !state.attached {
            self.attach(state);
        }
    }

    fn enqueue(&self, state: &mut FeedState, source: Box<dyn Source + Send>) {
        if state.format.is_some() && !state.closed {
            state.sources.push_back(source);
            self.wake(state);
        }
    }

    pub fn set_mixer(&self, mixer: Option<Mixer>) {
        let mut state = self.0.lock();
        state.mixer = mixer;
        self.attach(&mut state);
    }

    pub fn set_gain(&self, gain: f32) {
        self.0.lock().gain = gain;
    }

    pub fn pause(&self, paused: bool) {
        let mut state = self.0.lock();
        state.paused = paused;
        self.wake(&mut state);
    }

    pub fn reset(&self, channels: ChannelCount, sample_rate: SampleRate) {
        let mut state = self.0.lock();
        state.sources.clear();
        state.played = 0;
        state.paused = false;
        if state.format != Some((channels, sample_rate)) {
            state.format = Some((channels, sample_rate));
            self.attach(&mut state);
        }
    }

    pub fn append(&self, source: impl Source + Send + 'static) {
        self.enqueue(&mut self.0.lock(), Box::new(source));
    }

    pub fn clear(&self) {
        let mut state = self.0.lock();
        state.sources.clear();
        state.played = 0;
    }

    pub fn played(&self) -> Duration {
        let state = self.0.lock();
        let Some((channels, rate)) = state.format else {
            return Duration::ZERO;
        };
        let frames = state.played / u64::from(channels.get());
        Duration::from_nanos(frames * 1_000_000_000 / u64::from(rate.get()))
    }

    pub fn close(&self) {
        let mut state = self.0.lock();
        state.closed = true;
        state.sources.clear();
        self.attach(&mut state);
    }
}

impl VideoAudio for AudioFeed {
    fn reset(&mut self, sample_rate: u32, channels: u16) {
        if let (Some(channels), Some(rate)) = (NonZero::new(channels), NonZero::new(sample_rate)) {
            AudioFeed::reset(self, channels, rate);
        }
    }

    fn push(&mut self, samples: Vec<f32>) {
        let mut state = self.0.lock();
        if let Some((channels, rate)) = state.format {
            let source = Box::new(SamplesBuffer::new(channels, rate, samples));
            self.enqueue(&mut state, source);
        }
    }

    fn pause(&mut self, paused: bool) {
        AudioFeed::pause(self, paused);
    }
}

struct FeedSource {
    feed: AudioFeed,
    epoch: u64,
    format: Format,
    block: Vec<f32>,
    cursor: usize,
}

impl FeedSource {
    fn refill(&mut self) -> bool {
        let mut state = self.feed.0.lock();
        if state.epoch != self.epoch {
            return false;
        }
        if state.idle() {
            state.attached = false;
            return false;
        }
        let gain = state.gain;
        let samples = BLOCK_FRAMES * usize::from(self.format.0.get());
        self.block.clear();
        self.block
            .extend((0..samples).map(|_| state.pull().unwrap_or(0.0) * gain));
        self.cursor = 0;
        true
    }
}

impl Iterator for FeedSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.cursor == self.block.len() && !self.refill() {
            return None;
        }
        self.cursor += 1;
        self.block.get(self.cursor - 1).copied()
    }
}

impl Source for FeedSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        self.format.0
    }

    fn sample_rate(&self) -> SampleRate {
        self.format.1
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}
