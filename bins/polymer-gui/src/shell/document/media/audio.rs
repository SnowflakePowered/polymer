use super::{MediaPlayback, MediaSignalKind, NodeSignal, feed::AudioFeed, status::MediaStatus};
use crate::shell::protocol::ShellChannel;
use blitz_dom::NodeId;
use blitz_traits::net::{Bytes, Url};
use rodio::{Decoder, Source, mixer::Mixer, source::EmptyCallback};
use std::{io::Cursor, time::Duration};

const TIME_UPDATE_PERIOD: Duration = Duration::from_millis(250);

pub(super) struct DecodedSource {
    decoder: Decoder<Cursor<Bytes>>,
    offset: Duration,
    duration: Option<Duration>,
}

impl DecodedSource {
    fn open(bytes: &Bytes, offset: Duration) -> Option<Box<Self>> {
        let mut decoder = Self::decoder(bytes)?;
        let duration = decoder.total_duration();
        if offset.is_zero() || decoder.try_seek(offset).is_ok() {
            return Some(Box::new(Self {
                decoder,
                offset,
                duration,
            }));
        }
        Some(Box::new(Self {
            decoder: Self::decoder(bytes)?,
            offset: Duration::ZERO,
            duration,
        }))
    }

    fn decoder(bytes: &Bytes) -> Option<Decoder<Cursor<Bytes>>> {
        Decoder::builder()
            .with_byte_len(bytes.len() as u64)
            .with_data(Cursor::new(bytes.clone()))
            .with_seekable(true)
            .build()
            .inspect_err(|error| tracing::warn!(%error, "polymer: decoding shell audio failed"))
            .ok()
    }
}

pub(super) struct AudioPlayback {
    node: NodeId,
    shell: ShellChannel,
    feed: AudioFeed,
    bytes: Option<Bytes>,
    released: bool,
    installed: bool,
    offset: Duration,
}

impl AudioPlayback {
    pub fn new(node: NodeId, shell: ShellChannel, mixer: Option<&Mixer>) -> Self {
        Self {
            node,
            shell,
            feed: AudioFeed::new(mixer.cloned(), 1.0),
            bytes: None,
            released: false,
            installed: false,
            offset: Duration::ZERO,
        }
    }

    fn stop(&mut self) {
        self.installed = false;
        self.feed.clear();
    }

    fn release(&mut self, status: &MediaStatus) {
        if status.source.is_some() {
            self.bytes = None;
            self.released = true;
        }
    }

    fn decode(&mut self, status: &mut MediaStatus, generation: u64) {
        let Some(bytes) = self.bytes.clone() else {
            return;
        };
        self.stop();
        status.generation = generation;
        let (node, offset, shell) = (self.node, self.offset, self.shell.clone());
        let spawned = std::thread::Builder::new()
            .name("polymer-media-decode".into())
            .spawn(move || {
                let source = DecodedSource::open(&bytes, offset);
                NodeSignal::send(&shell, node, generation, MediaSignalKind::Decoded(source));
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "polymer: starting the audio decoder failed");
        }
    }

    fn install(&mut self, source: DecodedSource, status: &MediaStatus) {
        let (node, generation) = (self.node, status.generation);
        let (ticks, ended) = (self.shell.clone(), self.shell.clone());
        self.feed
            .reset(source.decoder.channels(), source.decoder.sample_rate());
        self.feed.pause(status.paused);
        self.feed.append(
            source
                .decoder
                .periodic_access(TIME_UPDATE_PERIOD, move |_| {
                    NodeSignal::send(&ticks, node, generation, MediaSignalKind::TimeUpdate);
                }),
        );
        self.feed.append(EmptyCallback::new(Box::new(move || {
            NodeSignal::send(&ended, node, generation, MediaSignalKind::Ended);
        })));
        self.offset = source.offset;
        self.installed = true;
    }

    fn ended(
        &mut self,
        status: &mut MediaStatus,
        generation: u64,
        connected: bool,
    ) -> Vec<&'static str> {
        if status.looping {
            self.offset = Duration::ZERO;
            self.decode(status, generation);
            return vec!["seeking", "timeupdate", "seeked"];
        }
        self.offset = self.position(status);
        self.stop();
        if !connected {
            self.release(status);
        }
        status.end()
    }
}

impl MediaPlayback for AudioPlayback {
    fn reload(&mut self, _status: &MediaStatus) {
        self.stop();
        self.bytes = None;
        self.released = false;
        self.offset = Duration::ZERO;
    }

    fn play(&mut self, status: &mut MediaStatus, generation: u64) -> Option<Url> {
        if status.resume()? {
            self.offset = Duration::ZERO;
            self.stop();
        }
        if std::mem::take(&mut self.released) {
            return status.source.clone();
        }
        if self.installed {
            self.feed.pause(false);
        } else {
            self.decode(status, generation);
        }
        None
    }

    fn pause(&mut self) {
        self.feed.pause(true);
    }

    fn seek(&mut self, status: &mut MediaStatus, position: Duration, generation: u64) {
        self.offset = position;
        self.stop();
        if !status.paused {
            self.decode(status, generation);
        }
    }

    fn configure(&mut self, status: &MediaStatus) {
        self.feed.set_gain(status.gain());
    }

    fn set_mixer(&mut self, mixer: Option<&Mixer>) {
        self.feed.set_mixer(mixer.cloned());
    }

    fn signal(
        &mut self,
        status: &mut MediaStatus,
        kind: MediaSignalKind,
        generation: u64,
        connected: bool,
    ) -> Vec<&'static str> {
        match kind {
            MediaSignalKind::Fetched(Some(bytes)) => {
                self.bytes = Some(bytes);
                self.released = false;
                self.decode(status, generation);
                Vec::new()
            }
            MediaSignalKind::Decoded(Some(source)) => {
                let kinds = status.loaded(source.duration);
                self.install(*source, status);
                kinds
            }
            MediaSignalKind::Fetched(None) | MediaSignalKind::Decoded(None) => {
                self.stop();
                status.fail()
            }
            MediaSignalKind::TimeUpdate => vec!["timeupdate"],
            MediaSignalKind::Ended => self.ended(status, generation, connected),
            MediaSignalKind::Video(_) | MediaSignalKind::Detached => Vec::new(),
        }
    }

    fn detach(&mut self, status: &MediaStatus) {
        self.offset = self.position(status);
        self.stop();
        self.release(status);
    }

    fn position(&self, status: &MediaStatus) -> Duration {
        if status.ended {
            return status.duration.unwrap_or(self.offset);
        }
        status.clamp(self.offset + self.feed.played())
    }
}

impl Drop for AudioPlayback {
    fn drop(&mut self) {
        self.feed.close();
    }
}
