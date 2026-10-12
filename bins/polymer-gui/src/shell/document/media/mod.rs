mod audio;
mod feed;
mod output;
mod status;
mod video;

use super::{ShellDocument, urls::ShellUrls};
use crate::shell::protocol::{ShellCommand, ShellInbox, ShellOutbox};
use audio::{AudioPlayback, DecodedSource};
use blitz_dom::{LocalName, Node, NodeId, local_name, node::ElementData};
use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request, Url};
use output::MediaOutput;
use polymer_composition::VideoEvent;
use rodio::mixer::Mixer;
use serde::Serialize;
use status::MediaStatus;
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};
use video::VideoPlayback;

pub(crate) enum MediaSignal {
    Node(NodeSignal),
    Output(Option<Mixer>),
}

pub(crate) struct NodeSignal {
    node: NodeId,
    generation: u64,
    kind: MediaSignalKind,
}

#[derive(Clone)]
enum MediaData {
    Memory(Bytes),
    File(PathBuf),
}

enum MediaSignalKind {
    Fetched(Option<MediaData>),
    Decoded(Option<Box<DecodedSource>>),
    TimeUpdate,
    Ended,
    Detached,
    Video(VideoEvent),
}

impl NodeSignal {
    fn send(shell: &ShellInbox, node: NodeId, generation: u64, kind: MediaSignalKind) {
        shell.send(ShellCommand::Media(MediaSignal::Node(Self {
            node,
            generation,
            kind,
        })));
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
    width: u32,
    height: u32,
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
    shell: ShellInbox,
    node: NodeId,
    generation: u64,
    delivered: bool,
}

impl NetHandler for MediaFetch {
    fn bytes(mut self: Box<Self>, _resolved_url: String, bytes: Bytes) {
        self.delivered = true;
        let kind = MediaSignalKind::Fetched(Some(MediaData::Memory(bytes)));
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

trait MediaPlayback {
    fn reload(&mut self, status: &MediaStatus);
    fn play(&mut self, status: &mut MediaStatus, generation: u64) -> Option<Url>;
    fn pause(&mut self);
    fn seek(&mut self, status: &mut MediaStatus, position: Duration, generation: u64);
    fn configure(&mut self, status: &MediaStatus);
    fn set_looping(&mut self, _status: &MediaStatus) {}
    fn set_mixer(&mut self, mixer: Option<&Mixer>);
    fn signal(
        &mut self,
        status: &mut MediaStatus,
        kind: MediaSignalKind,
        generation: u64,
        connected: bool,
    ) -> Vec<&'static str>;
    fn detach(&mut self, status: &MediaStatus);
    fn position(&self, status: &MediaStatus) -> Duration;
    fn size(&self) -> (u32, u32) {
        (0, 0)
    }
}

struct MediaElement {
    status: MediaStatus,
    playback: Box<dyn MediaPlayback>,
}

impl MediaElement {
    fn state(&self) -> MediaState {
        let position = self.playback.position(&self.status);
        self.status.state(position, self.playback.size())
    }
}

pub(crate) struct MediaPlayers {
    net: Arc<dyn NetProvider>,
    doc_id: usize,
    base: Option<Url>,
    shell: ShellInbox,
    outbox: ShellOutbox,
    urls: Arc<ShellUrls>,
    _output: MediaOutput,
    mixer: Option<Mixer>,
    generation: u64,
    elements: HashMap<NodeId, MediaElement>,
}

impl MediaPlayers {
    pub(super) fn contains(&self, node: NodeId) -> bool {
        self.elements.contains_key(&node)
    }

    pub(super) fn new(
        net: Arc<dyn NetProvider>,
        doc_id: usize,
        base_url: &str,
        shell: ShellInbox,
        outbox: ShellOutbox,
        urls: Arc<ShellUrls>,
    ) -> Self {
        Self {
            net,
            doc_id,
            base: Url::parse(base_url).ok(),
            _output: MediaOutput::spawn(shell.clone()),
            shell,
            outbox,
            urls,
            mixer: None,
            generation: 0,
            elements: HashMap::new(),
        }
    }

    fn next_generation(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    fn fetch(&self, node: NodeId, generation: u64, url: Url) {
        let resolved = self.urls.resolve(&url);
        if resolved.as_ref().is_none_or(|url| url.scheme() == "file") {
            let file = resolved
                .and_then(|url| url.to_file_path().ok())
                .filter(|path| path.is_file())
                .map(MediaData::File);
            NodeSignal::send(&self.shell, node, generation, MediaSignalKind::Fetched(file));
            return;
        }
        let url = resolved.unwrap_or(url);
        let fetch = MediaFetch {
            shell: self.shell.clone(),
            node,
            generation,
            delivered: false,
        };
        self.net.fetch(self.doc_id, Request::get(url), Box::new(fetch));
    }

    fn element(&mut self, node: NodeId, video: bool, muted: bool) -> &mut MediaElement {
        self.elements.entry(node).or_insert_with(|| {
            let playback: Box<dyn MediaPlayback> = if video {
                Box::new(VideoPlayback::new(
                    node,
                    self.shell.clone(),
                    self.outbox.clone(),
                    self.mixer.as_ref(),
                ))
            } else {
                Box::new(AudioPlayback::new(node, self.shell.clone(), self.mixer.as_ref()))
            };
            MediaElement {
                status: MediaStatus::new(muted),
                playback,
            }
        })
    }

    fn load(&mut self, node: NodeId, video: bool, attributes: MediaAttributes) -> bool {
        if attributes.source.is_none() && !self.elements.contains_key(&node) {
            return false;
        }
        let generation = self.next_generation();
        let source = attributes
            .source
            .as_deref()
            .and_then(|source| Url::options().base_url(self.base.as_ref()).parse(source).ok());
        let element = self.element(node, video, attributes.muted);
        element.status.reload(generation, source.clone(), &attributes);
        element.playback.reload(&element.status);
        element.playback.configure(&element.status);
        let Some(url) = source else {
            return false;
        };
        self.fetch(node, generation, url);
        true
    }

    pub(crate) fn play(&mut self, node: NodeId) {
        let generation = self.next_generation();
        let Some(element) = self.elements.get_mut(&node) else {
            return;
        };
        if let Some(url) = element.playback.play(&mut element.status, generation) {
            element.status.generation = generation;
            self.fetch(node, generation, url);
        }
    }

    pub(crate) fn pause(&mut self, node: NodeId) -> f64 {
        let Some(element) = self.elements.get_mut(&node) else {
            return 0.0;
        };
        element.status.paused = true;
        element.playback.pause();
        element.playback.position(&element.status).as_secs_f64()
    }

    pub(crate) fn seek(&mut self, node: NodeId, seconds: f64) -> f64 {
        let generation = self.next_generation();
        let Some(element) = self.elements.get_mut(&node) else {
            return 0.0;
        };
        let target = Duration::try_from_secs_f64(seconds).unwrap_or_default();
        let position = element.status.clamp(target);
        element.status.ended = false;
        element.playback.seek(&mut element.status, position, generation);
        position.as_secs_f64()
    }

    fn configure(&mut self, node: NodeId, video: bool, volume: f32, muted: bool) {
        let element = self.element(node, video, muted);
        element.status.volume = volume.clamp(0.0, 1.0);
        element.status.muted = muted;
        element.playback.configure(&element.status);
    }

    pub(crate) fn state(&self, node: NodeId) -> Option<MediaState> {
        self.elements.get(&node).map(MediaElement::state)
    }

    fn output_changed(&mut self, mixer: Option<Mixer>) {
        self.mixer = mixer;
        for element in self.elements.values_mut() {
            element.playback.set_mixer(self.mixer.as_ref());
        }
    }

    fn signal(&mut self, signal: NodeSignal, connected: bool) -> Option<MediaEvents> {
        let NodeSignal {
            node,
            generation,
            kind,
        } = signal;
        let fresh = self.next_generation();
        let element = self.elements.get_mut(&node)?;
        let (status, playback) = (&mut element.status, &mut element.playback);
        let kinds = match kind {
            MediaSignalKind::Detached if connected => return None,
            MediaSignalKind::Detached => {
                playback.detach(status);
                status.halt(fresh)
            }
            _ if status.generation != generation => return None,
            kind => playback.signal(status, kind, fresh, connected),
        };
        let state = element.state();
        Some(MediaEvents { node, kinds, state })
    }
}

impl ShellDocument {
    pub(crate) fn media(&mut self) -> &mut MediaPlayers {
        &mut self.media
    }

    fn media_element(&self, node: NodeId) -> Option<(bool, &ElementData)> {
        let element = self.document.as_ref().get_node(node)?.element_data()?;
        let video = element.name.local == local_name!("video");
        (video || element.name.local == local_name!("audio")).then_some((video, element))
    }

    pub(crate) fn load_media(&mut self, node: NodeId) -> bool {
        let Some((video, element)) = self.media_element(node) else {
            return false;
        };
        let attributes = MediaAttributes {
            source: element.attr(local_name!("src")).map(str::to_owned),
            autoplay: element.attr(local_name!("autoplay")).is_some(),
            looping: element.attr(local_name!("loop")).is_some(),
            muted: element.attr(local_name!("muted")).is_some(),
        };
        self.media.load(node, video, attributes)
    }

    pub(crate) fn configure_media(&mut self, node: NodeId, volume: f32, muted: bool) {
        if let Some((video, _)) = self.media_element(node) {
            self.media.configure(node, video, volume, muted);
        }
    }

    pub(super) fn load_document_media(&mut self) {
        let nodes = self
            .document
            .as_ref()
            .tree()
            .iter()
            .map(|(node, _)| node)
            .filter(|node| self.media_element(*node).is_some())
            .collect::<Vec<_>>();
        for node in nodes {
            self.load_media(node);
        }
    }

    pub(super) fn media_attribute_changed(&mut self, node: NodeId, name: &LocalName) {
        if *name == local_name!("src") {
            self.load_media(node);
            return;
        }
        if *name != local_name!("loop") {
            return;
        }
        let looping = self
            .document
            .as_ref()
            .get_node(node)
            .and_then(Node::element_data)
            .is_some_and(|element| element.attr(local_name!("loop")).is_some());
        if let Some(element) = self.media.elements.get_mut(&node) {
            element.status.looping = looping;
            element.playback.set_looping(&element.status);
        }
    }

    pub(super) fn detach_media(&self, root: NodeId) {
        let document = self.document.as_ref();
        for node in self.media.elements.keys().copied() {
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
            self.media.elements.remove(&id);
        }
        events
    }
}
