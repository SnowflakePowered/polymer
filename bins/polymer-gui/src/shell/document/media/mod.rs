mod output;

use super::ShellDocument;
use crate::shell::protocol::{ShellChannel, ShellCommand};
use output::MediaOutput;
use blitz_dom::{LocalName, Node, NodeId, local_name};
use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request, Url};
use rodio::{Decoder, Player, Source, mixer::Mixer, source::EmptyCallback};
use serde::Serialize;
use std::{collections::HashMap, io::Cursor, sync::Arc, time::Duration};

const TIME_UPDATE_PERIOD: Duration = Duration::from_millis(250);
const READY_EVENTS: [&str; 5] = [
    "durationchange",
    "loadedmetadata",
    "loadeddata",
    "canplay",
    "canplaythrough",
];

pub(crate) enum MediaSignal {
    Node(NodeSignal),
    Output(Option<Mixer>),
}

pub(crate) struct NodeSignal {
    node: NodeId,
    generation: u64,
    kind: MediaSignalKind,
}

enum MediaSignalKind {
    Fetched(Option<Bytes>),
    Decoded(Option<Box<DecodedSource>>),
    TimeUpdate,
    Ended,
    Detached,
}

impl NodeSignal {
    fn send(shell: &ShellChannel, node: NodeId, generation: u64, kind: MediaSignalKind) {
        shell.send(ShellCommand::Media(MediaSignal::Node(Self {
            node,
            generation,
            kind,
        })));
    }
}

struct DecodedSource {
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MediaState {
    source: Option<String>,
    current_time: f64,
    duration: Option<f64>,
    paused: bool,
    ended: bool,
    ready: bool,
    error: bool,
    volume: f32,
    muted: bool,
}

pub(crate) struct MediaEvents {
    pub node: NodeId,
    pub kinds: Vec<&'static str>,
    pub state: MediaState,
}

struct MediaAttributes {
    source: Option<String>,
    autoplay: bool,
    looping: bool,
    muted: bool,
}

struct MediaFetch {
    shell: ShellChannel,
    node: NodeId,
    generation: u64,
    delivered: bool,
}

impl NetHandler for MediaFetch {
    fn bytes(mut self: Box<Self>, _resolved_url: String, bytes: Bytes) {
        self.delivered = true;
        let kind = MediaSignalKind::Fetched(Some(bytes));
        NodeSignal::send(&self.shell, self.node, self.generation, kind);
    }
}

impl Drop for MediaFetch {
    fn drop(&mut self) {
        if !self.delivered {
            let kind = MediaSignalKind::Fetched(None);
            NodeSignal::send(&self.shell, self.node, self.generation, kind);
        }
    }
}

struct MediaEntry {
    generation: u64,
    source: Option<Url>,
    bytes: Option<Bytes>,
    released: bool,
    player: Option<Player>,
    duration: Option<Duration>,
    offset: Duration,
    paused: bool,
    ended: bool,
    ready: bool,
    error: bool,
    looping: bool,
    volume: f32,
    muted: bool,
}

impl Default for MediaEntry {
    fn default() -> Self {
        Self {
            generation: 0,
            source: None,
            bytes: None,
            released: false,
            player: None,
            duration: None,
            offset: Duration::ZERO,
            paused: true,
            ended: false,
            ready: false,
            error: false,
            looping: false,
            volume: 1.0,
            muted: false,
        }
    }
}

impl MediaEntry {
    fn position(&self) -> Duration {
        match &self.player {
            Some(player) if player.len() > 1 => self.offset + player.get_pos(),
            Some(_) => self.duration.unwrap_or(self.offset),
            None => self.offset,
        }
    }

    fn state(&self) -> MediaState {
        MediaState {
            source: self.source.as_ref().map(Url::to_string),
            current_time: self.position().as_secs_f64(),
            duration: self.duration.map(|duration| duration.as_secs_f64()),
            paused: self.paused,
            ended: self.ended,
            ready: self.ready,
            error: self.error,
            volume: self.volume,
            muted: self.muted,
        }
    }

    fn gain(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume }
    }

    fn stop(&mut self, generation: u64) {
        if self.player.is_some() {
            self.offset = self.position();
            self.player = None;
            self.generation = generation;
        }
    }

    fn release(&mut self) {
        if self.source.is_some() {
            self.bytes = None;
            self.released = true;
        }
    }

    fn fail(&mut self) -> Vec<&'static str> {
        self.player = None;
        self.paused = true;
        self.error = true;
        vec!["error"]
    }

    fn decode(&mut self, node: NodeId, generation: u64, shell: &ShellChannel) {
        let Some(bytes) = self.bytes.clone() else {
            return;
        };
        self.generation = generation;
        let (offset, shell) = (self.offset, shell.clone());
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

    fn install(&mut self, node: NodeId, source: DecodedSource, shell: &ShellChannel, mixer: &Mixer) {
        let generation = self.generation;
        let player = Player::connect_new(mixer);
        player.set_volume(self.gain());
        let ticks = shell.clone();
        player.append(source.decoder.periodic_access(TIME_UPDATE_PERIOD, move |_| {
            NodeSignal::send(&ticks, node, generation, MediaSignalKind::TimeUpdate);
        }));
        let ended = shell.clone();
        player.append(EmptyCallback::new(Box::new(move || {
            NodeSignal::send(&ended, node, generation, MediaSignalKind::Ended);
        })));
        self.offset = source.offset;
        self.player = Some(player);
    }
}

pub(crate) struct MediaPlayers {
    net: Arc<dyn NetProvider>,
    doc_id: usize,
    base: Option<Url>,
    shell: ShellChannel,
    _output: MediaOutput,
    mixer: Option<Mixer>,
    generation: u64,
    entries: HashMap<NodeId, MediaEntry>,
}

impl MediaPlayers {
    pub(super) fn new(
        net: Arc<dyn NetProvider>,
        doc_id: usize,
        base_url: &str,
        shell: ShellChannel,
    ) -> Self {
        Self {
            net,
            doc_id,
            base: Url::parse(base_url).ok(),
            _output: MediaOutput::spawn(shell.clone()),
            shell,
            mixer: None,
            generation: 0,
            entries: HashMap::new(),
        }
    }

    fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    fn fetch(&self, node: NodeId, generation: u64, url: Url) {
        let fetch = MediaFetch {
            shell: self.shell.clone(),
            node,
            generation,
            delivered: false,
        };
        self.net.fetch(self.doc_id, Request::get(url), Box::new(fetch));
    }

    fn load(&mut self, node: NodeId, attributes: MediaAttributes) -> bool {
        if attributes.source.is_none() && !self.entries.contains_key(&node) {
            return false;
        }
        let generation = self.next_generation();
        let entry = self.entries.entry(node).or_insert_with(|| MediaEntry {
            muted: attributes.muted,
            ..MediaEntry::default()
        });
        let source = attributes
            .source
            .and_then(|source| Url::options().base_url(self.base.as_ref()).parse(&source).ok());
        *entry = MediaEntry {
            generation,
            paused: !attributes.autoplay || source.is_none(),
            error: source.is_none(),
            source: source.clone(),
            looping: attributes.looping,
            volume: entry.volume,
            muted: entry.muted,
            ..MediaEntry::default()
        };
        let Some(url) = source else {
            return false;
        };
        self.fetch(node, generation, url);
        true
    }

    pub(crate) fn play(&mut self, node: NodeId) {
        let generation = self.next_generation();
        let Some(entry) = self.entries.get_mut(&node).filter(|entry| entry.paused) else {
            return;
        };
        entry.paused = false;
        if entry.ended {
            entry.ended = false;
            entry.offset = Duration::ZERO;
        }
        match entry.source.clone().filter(|_| entry.released) {
            Some(url) => {
                entry.released = false;
                entry.generation = generation;
                self.fetch(node, generation, url);
            }
            None => entry.decode(node, generation, &self.shell),
        }
    }

    pub(crate) fn pause(&mut self, node: NodeId) -> f64 {
        let generation = self.next_generation();
        let Some(entry) = self.entries.get_mut(&node) else {
            return 0.0;
        };
        entry.paused = true;
        entry.stop(generation);
        entry.offset.as_secs_f64()
    }

    pub(crate) fn seek(&mut self, node: NodeId, seconds: f64) -> f64 {
        let generation = self.next_generation();
        let Some(entry) = self.entries.get_mut(&node) else {
            return 0.0;
        };
        let target = Duration::try_from_secs_f64(seconds).unwrap_or_default();
        entry.offset = entry.duration.map_or(target, |duration| target.min(duration));
        entry.ended = false;
        if !entry.paused {
            entry.player = None;
            entry.decode(node, generation, &self.shell);
        }
        entry.offset.as_secs_f64()
    }

    pub(crate) fn configure(&mut self, node: NodeId, volume: f32, muted: bool) {
        let entry = self.entries.entry(node).or_default();
        entry.volume = volume.clamp(0.0, 1.0);
        entry.muted = muted;
        if let Some(player) = &entry.player {
            player.set_volume(entry.gain());
        }
    }

    pub(crate) fn state(&self, node: NodeId) -> Option<MediaState> {
        self.entries.get(&node).map(MediaEntry::state)
    }

    fn output_changed(&mut self, mixer: Option<Mixer>) {
        self.mixer = mixer;
        if self.mixer.is_none() {
            return;
        }
        let playing = self
            .entries
            .iter()
            .filter(|(_, entry)| !entry.paused)
            .map(|(node, _)| *node)
            .collect::<Vec<_>>();
        for node in playing {
            let generation = self.next_generation();
            if let Some(entry) = self.entries.get_mut(&node) {
                entry.stop(generation);
                entry.decode(node, generation, &self.shell);
            }
        }
    }

    fn signal(&mut self, signal: NodeSignal, connected: bool) -> Option<MediaEvents> {
        let NodeSignal {
            node,
            generation,
            kind,
        } = signal;
        let current = self.entries.get(&node)?.generation == generation;
        let kinds = match kind {
            MediaSignalKind::Detached if connected => return None,
            MediaSignalKind::Detached => self.detached(node),
            _ if !current => return None,
            MediaSignalKind::Fetched(bytes) => self.fetched(node, bytes),
            MediaSignalKind::Decoded(source) => self.decoded(node, source),
            MediaSignalKind::TimeUpdate => vec!["timeupdate"],
            MediaSignalKind::Ended => self.ended(node, connected),
        };
        let state = self.entries.get(&node)?.state();
        Some(MediaEvents { node, kinds, state })
    }

    fn fetched(&mut self, node: NodeId, bytes: Option<Bytes>) -> Vec<&'static str> {
        let generation = self.next_generation();
        let Some(entry) = self.entries.get_mut(&node) else {
            return Vec::new();
        };
        let Some(bytes) = bytes else {
            return entry.fail();
        };
        entry.bytes = Some(bytes);
        entry.released = false;
        entry.decode(node, generation, &self.shell);
        Vec::new()
    }

    fn decoded(&mut self, node: NodeId, source: Option<Box<DecodedSource>>) -> Vec<&'static str> {
        let Some(entry) = self.entries.get_mut(&node) else {
            return Vec::new();
        };
        let Some(source) = source else {
            return entry.fail();
        };
        let mut kinds = if entry.ready { Vec::new() } else { READY_EVENTS.to_vec() };
        entry.ready = true;
        entry.duration = source.duration;
        if entry.paused {
            return kinds;
        }
        if let Some(mixer) = &self.mixer {
            entry.install(node, *source, &self.shell, mixer);
        }
        if !kinds.is_empty() {
            kinds.push("playing");
        }
        kinds
    }

    fn ended(&mut self, node: NodeId, connected: bool) -> Vec<&'static str> {
        let generation = self.next_generation();
        let Some(entry) = self.entries.get_mut(&node) else {
            return Vec::new();
        };
        entry.stop(generation);
        if entry.looping {
            entry.offset = Duration::ZERO;
            entry.decode(node, generation, &self.shell);
            return vec!["seeking", "timeupdate", "seeked"];
        }
        entry.paused = true;
        entry.ended = true;
        if !connected {
            entry.release();
        }
        vec!["timeupdate", "pause", "ended"]
    }

    fn detached(&mut self, node: NodeId) -> Vec<&'static str> {
        let generation = self.next_generation();
        let Some(entry) = self.entries.get_mut(&node) else {
            return Vec::new();
        };
        let playing = !entry.paused;
        entry.paused = true;
        entry.stop(generation);
        entry.release();
        if playing { vec!["pause"] } else { Vec::new() }
    }
}

impl ShellDocument {
    pub(crate) fn media(&mut self) -> &mut MediaPlayers {
        &mut self.media
    }

    pub(crate) fn load_media(&mut self, node: NodeId) -> bool {
        let Some(element) = self.document.as_ref().get_node(node).and_then(Node::element_data)
        else {
            return false;
        };
        if element.name.local != local_name!("audio") {
            return false;
        }
        let attributes = MediaAttributes {
            source: element.attr(local_name!("src")).map(str::to_owned),
            autoplay: element.attr(local_name!("autoplay")).is_some(),
            looping: element.attr(local_name!("loop")).is_some(),
            muted: element.attr(local_name!("muted")).is_some(),
        };
        self.media.load(node, attributes)
    }

    pub(super) fn load_document_media(&mut self) {
        let nodes = self
            .document
            .as_ref()
            .tree()
            .iter()
            .filter(|(_, node)| {
                node.element_data()
                    .is_some_and(|element| element.name.local == local_name!("audio"))
            })
            .map(|(node, _)| node)
            .collect::<Vec<_>>();
        for node in nodes {
            self.load_media(node);
        }
    }

    pub(super) fn media_attribute_changed(&mut self, node: NodeId, name: &LocalName) {
        if *name == local_name!("src") {
            self.load_media(node);
        } else if *name == local_name!("loop")
            && let Some(entry) = self.media.entries.get_mut(&node)
        {
            entry.looping = self
                .document
                .as_ref()
                .get_node(node)
                .and_then(Node::element_data)
                .is_some_and(|element| element.attr(local_name!("loop")).is_some());
        }
    }

    pub(super) fn detach_media(&self, root: NodeId) {
        let document = self.document.as_ref();
        for node in self.media.entries.keys().copied() {
            let removed = std::iter::successors(Some(node), |node| {
                document.get_node(*node).and_then(|node| node.parent)
            })
            .any(|node| node == root);
            if removed {
                NodeSignal::send(&self.media.shell, node, 0, MediaSignalKind::Detached);
            }
        }
    }

    pub(crate) fn media_signal(&mut self, signal: MediaSignal) -> Option<MediaEvents> {
        let signal = match signal {
            MediaSignal::Node(signal) => signal,
            MediaSignal::Output(mixer) => {
                self.media.output_changed(mixer);
                return None;
            }
        };
        let id = signal.node;
        let node = self.document.as_ref().get_node(id);
        let exists = node.is_some();
        let connected = node.is_some_and(|node| node.flags.is_in_document());
        let events = self.media.signal(signal, connected);
        if !exists {
            self.media.entries.remove(&id);
        }
        events
    }
}
