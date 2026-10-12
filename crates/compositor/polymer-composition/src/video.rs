use std::{sync::Arc, time::Duration};

#[derive(Clone, Copy, Debug)]
pub enum VideoControl {
    Play,
    Pause,
    Seek(Duration),
    Loop(bool),
}

#[derive(Clone, Debug)]
pub enum VideoEvent {
    Ready {
        duration: Option<Duration>,
        width: u32,
        height: u32,
    },
    Position(Duration),
    Seeked,
    Ended,
    Failed(String),
}

pub trait VideoAudio: Send {
    fn reset(&mut self, sample_rate: u32, channels: u16);
    fn push(&mut self, samples: Vec<f32>);
    fn pause(&mut self, paused: bool);
}

pub struct VideoSink {
    pub events: Arc<dyn Fn(VideoEvent) + Send + Sync>,
    pub audio: Box<dyn VideoAudio>,
}

pub type VideoBytes = Box<dyn AsRef<[u8]> + Send>;
