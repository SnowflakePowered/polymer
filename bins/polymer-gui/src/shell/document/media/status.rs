use super::{MediaAttributes, MediaState};
use blitz_traits::net::Url;
use std::time::Duration;

const READY_EVENTS: [&str; 5] = [
    "durationchange",
    "loadedmetadata",
    "loadeddata",
    "canplay",
    "canplaythrough",
];

pub(super) struct MediaStatus {
    pub generation: u64,
    pub source: Option<Url>,
    pub duration: Option<Duration>,
    pub paused: bool,
    pub ended: bool,
    pub ready: bool,
    pub error: bool,
    pub looping: bool,
    pub volume: f32,
    pub muted: bool,
}

impl MediaStatus {
    pub fn new(muted: bool) -> Self {
        Self {
            generation: 0,
            source: None,
            duration: None,
            paused: true,
            ended: false,
            ready: false,
            error: false,
            looping: false,
            volume: 1.0,
            muted,
        }
    }

    pub fn reload(&mut self, generation: u64, source: Option<Url>, attributes: &MediaAttributes) {
        *self = Self {
            generation,
            paused: !attributes.autoplay || source.is_none(),
            error: source.is_none(),
            looping: attributes.looping,
            source,
            volume: self.volume,
            ..Self::new(self.muted)
        };
    }

    pub fn state(&self, position: Duration, (width, height): (u32, u32)) -> MediaState {
        MediaState {
            source: self.source.as_ref().map(Url::to_string),
            current_time: position.as_secs_f64(),
            duration: self.duration.map(|duration| duration.as_secs_f64()),
            paused: self.paused,
            ended: self.ended,
            ready: self.ready,
            error: self.error,
            volume: self.volume,
            muted: self.muted,
            width,
            height,
        }
    }

    pub fn gain(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume }
    }

    pub fn clamp(&self, position: Duration) -> Duration {
        self.duration
            .map_or(position, |duration| position.min(duration))
    }

    pub fn resume(&mut self) -> Option<bool> {
        if !self.paused {
            return None;
        }
        self.paused = false;
        Some(std::mem::take(&mut self.ended))
    }

    pub fn loaded(&mut self, duration: Option<Duration>) -> Vec<&'static str> {
        self.duration = duration;
        let mut kinds = if self.ready {
            Vec::new()
        } else {
            READY_EVENTS.to_vec()
        };
        self.ready = true;
        if !self.paused && !kinds.is_empty() {
            kinds.push("playing");
        }
        kinds
    }

    pub fn end(&mut self) -> Vec<&'static str> {
        self.paused = true;
        self.ended = true;
        vec!["timeupdate", "pause", "ended"]
    }

    pub fn fail(&mut self) -> Vec<&'static str> {
        self.paused = true;
        self.error = true;
        vec!["error"]
    }

    pub fn halt(&mut self, generation: u64) -> Vec<&'static str> {
        let playing = !self.paused;
        self.paused = true;
        self.generation = generation;
        if playing { vec!["pause"] } else { Vec::new() }
    }
}
