use super::{MediaPlayback, MediaSignalKind, NodeSignal, feed::AudioFeed, status::MediaStatus};
use crate::shell::{
    ShellOutbox,
    protocol::{ShellChannel, ShellEvent, TrackedSource, VideoRequest},
};
use blitz_dom::NodeId;
use blitz_traits::net::{Bytes, Url};
use polymer_composition::{VideoControl, VideoEvent, VideoSink};
use rodio::mixer::Mixer;
use std::{sync::Arc, time::Duration};

pub(super) struct VideoPlayback {
    node: NodeId,
    id: u64,
    shell: ShellChannel,
    outbox: ShellOutbox,
    mixer: Option<Mixer>,
    feed: Option<AudioFeed>,
    fetching: bool,
    seeks: u32,
    position: Duration,
    width: u32,
    height: u32,
}

impl VideoPlayback {
    pub fn new(
        node: NodeId,
        shell: ShellChannel,
        outbox: ShellOutbox,
        mixer: Option<&Mixer>,
    ) -> Self {
        Self {
            node,
            id: TrackedSource::video(node).id(),
            shell,
            outbox,
            mixer: mixer.cloned(),
            feed: None,
            fetching: false,
            seeks: 0,
            position: Duration::ZERO,
            width: 0,
            height: 0,
        }
    }

    fn control(&self, control: VideoControl) {
        if self.feed.is_some() {
            self.outbox.send(ShellEvent::Video(VideoRequest::Control {
                id: self.id,
                control,
            }));
        }
    }

    fn close(&mut self) {
        if let Some(feed) = self.feed.take() {
            feed.close();
            self.outbox
                .send(ShellEvent::Video(VideoRequest::Close { id: self.id }));
        }
    }

    fn open(&mut self, bytes: Option<Bytes>, status: &mut MediaStatus) -> Vec<&'static str> {
        self.fetching = false;
        let Some(bytes) = bytes else {
            return status.fail();
        };
        let (shell, node, generation) = (self.shell.clone(), self.node, status.generation);
        let events = Arc::new(move |event| {
            NodeSignal::send(&shell, node, generation, MediaSignalKind::Video(event));
        });
        let feed = AudioFeed::new(self.mixer.clone(), status.gain());
        let sink = VideoSink {
            events,
            audio: Box::new(feed.clone()),
        };
        self.feed = Some(feed);
        self.outbox.send(ShellEvent::Video(VideoRequest::Open {
            id: self.id,
            bytes: Box::new(bytes),
            sink,
        }));
        if !self.position.is_zero() {
            self.control(VideoControl::Seek(self.position));
        }
        self.control(VideoControl::Loop(status.looping));
        if !status.paused {
            self.control(VideoControl::Play);
        }
        Vec::new()
    }

    fn event(&mut self, event: VideoEvent, status: &mut MediaStatus) -> Vec<&'static str> {
        match event {
            VideoEvent::Ready {
                duration,
                width,
                height,
            } => {
                (self.width, self.height) = (width, height);
                self.outbox.send(ShellEvent::Video(VideoRequest::Extent {
                    id: self.id,
                    width,
                    height,
                }));
                status.loaded(duration)
            }
            VideoEvent::Position(_) if self.seeks > 0 => Vec::new(),
            VideoEvent::Position(position) => {
                self.position = position;
                vec!["timeupdate"]
            }
            VideoEvent::Seeked => {
                self.seeks = self.seeks.saturating_sub(1);
                Vec::new()
            }
            VideoEvent::Ended => {
                self.position = status.duration.unwrap_or(self.position);
                status.end()
            }
            VideoEvent::Failed(_) => {
                self.close();
                status.fail()
            }
        }
    }
}

impl MediaPlayback for VideoPlayback {
    fn reload(&mut self, status: &MediaStatus) {
        self.close();
        self.fetching = status.source.is_some();
        self.seeks = 0;
        self.position = Duration::ZERO;
        (self.width, self.height) = (0, 0);
    }

    fn play(&mut self, status: &mut MediaStatus, _generation: u64) -> Option<Url> {
        if status.resume()? {
            self.position = Duration::ZERO;
        }
        self.control(VideoControl::Play);
        if self.feed.is_some() || self.fetching {
            return None;
        }
        let source = status.source.clone()?;
        self.fetching = true;
        Some(source)
    }

    fn pause(&mut self) {
        self.control(VideoControl::Pause);
    }

    fn seek(&mut self, _status: &mut MediaStatus, position: Duration, _generation: u64) {
        self.position = position;
        if self.feed.is_some() {
            self.seeks += 1;
        }
        self.control(VideoControl::Seek(position));
    }

    fn configure(&mut self, status: &MediaStatus) {
        if let Some(feed) = &self.feed {
            feed.set_gain(status.gain());
        }
    }

    fn set_looping(&mut self, status: &MediaStatus) {
        self.control(VideoControl::Loop(status.looping));
    }

    fn set_mixer(&mut self, mixer: Option<&Mixer>) {
        self.mixer = mixer.cloned();
        if let Some(feed) = &self.feed {
            feed.set_mixer(self.mixer.clone());
        }
    }

    fn signal(
        &mut self,
        status: &mut MediaStatus,
        kind: MediaSignalKind,
        _generation: u64,
        _connected: bool,
    ) -> Vec<&'static str> {
        match kind {
            MediaSignalKind::Fetched(bytes) => self.open(bytes, status),
            MediaSignalKind::Video(event) => self.event(event, status),
            _ => Vec::new(),
        }
    }

    fn detach(&mut self, _status: &MediaStatus) {
        self.close();
    }

    fn position(&self, _status: &MediaStatus) -> Duration {
        self.position
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

impl Drop for VideoPlayback {
    fn drop(&mut self) {
        self.close();
    }
}
