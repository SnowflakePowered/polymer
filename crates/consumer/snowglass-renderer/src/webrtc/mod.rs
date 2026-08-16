//! WebRTC signaling and one-track-per-capture-stream publishing.
#![allow(unsafe_code)]

use crate::{CompositingPlan, D3d12Compositor, WebRtcCompositor};
use parking_lot::Mutex;
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::runtime::{Handle, Runtime};
use webrtc::{
    api::{
        API, APIBuilder,
        interceptor_registry::register_default_interceptors,
        media_engine::{MIME_TYPE_H264, MIME_TYPE_HEVC, MediaEngine},
        setting_engine::SettingEngine,
    },
    interceptor::registry::Registry,
    media::Sample,
    peer_connection::{
        RTCPeerConnection, configuration::RTCConfiguration,
        peer_connection_state::RTCPeerConnectionState,
        sdp::session_description::RTCSessionDescription,
    },
    rtp_transceiver::{
        rtp_codec::{RTCRtpCodecCapability, RTPCodecType},
        rtp_transceiver_direction::RTCRtpTransceiverDirection,
    },
    stats::StatsReportType,
    track::track_local::{TrackLocal, track_local_static_sample::TrackLocalStaticSample},
};
use webrtc_ice::{
    network_type::NetworkType,
    udp_mux::{UDPMuxDefault, UDPMuxParams},
    udp_network::UDPNetwork,
};
use windows::Win32::Graphics::{
    Direct3D12::{D3D12_RESOURCE_STATES, ID3D12Device, ID3D12Fence, ID3D12Resource},
    Dxgi::Common::DXGI_FORMAT,
};

const FRAME_RATE: f32 = 60.0;
const FRAME_DURATION: Duration = Duration::from_micros(16_667);
const DEFAULT_BITRATE: u32 = 8_000_000;
const MAX_HTTP_HEADER: usize = 64 * 1024;
/// Bounded frame handoff to the encoder thread. GPU-backed frame producers
/// must never have more frames outstanding than this, so an accepted frame
/// can never resolve after a later frame that was dropped by `try_send`.
pub(crate) const FRAME_CHANNEL_CAPACITY: usize = 2;
const ICE_GATHERING_TIMEOUT: Duration = Duration::from_secs(1);
static NEXT_WHEP_SESSION_ID: AtomicU64 = AtomicU64::new(1);

mod encoders;

pub struct VideoFrame {
    pub(crate) connection: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) data: VideoFrameData,
}

pub(crate) enum VideoFrameData {
    Rgba(Vec<u8>),
    D3d12(D3d12Frame),
    Vulkan(VulkanFrame),
}

pub(crate) struct VulkanFrame {
    pub(crate) instance: ash::Instance,
    pub(crate) physical_device: ash::vk::PhysicalDevice,
    pub(crate) device: ash::Device,
    pub(crate) queue: ash::vk::Queue,
    pub(crate) queue_family: u32,
    pub(crate) conversion_queue: ash::vk::Queue,
    pub(crate) conversion_queue_family: u32,
    pub(crate) image: ash::vk::Image,
    pub(crate) format: ash::vk::Format,
    pub(crate) layout: ash::vk::ImageLayout,
    pub(crate) ready: ash::vk::Semaphore,
    pub(crate) ready_value: u64,
    pub(crate) completion: ash::vk::Semaphore,
    pub(crate) completion_value: u64,
    pub(crate) completion_submitted: bool,
}

impl VulkanFrame {
    pub(crate) fn mark_completion_submitted(&mut self) {
        self.completion_submitted = true;
    }

    pub(crate) fn completion_submitted(&self) -> bool {
        self.completion_submitted
    }
}

impl Drop for VulkanFrame {
    fn drop(&mut self) {
        if !self.completion_submitted {
            let _ = unsafe {
                self.device.signal_semaphore(
                    &ash::vk::SemaphoreSignalInfo::default()
                        .semaphore(self.completion)
                        .value(self.completion_value),
                )
            };
        }
    }
}

pub(crate) struct D3d12Frame {
    pub(crate) device: ID3D12Device,
    pub(crate) resource: ID3D12Resource,
    pub(crate) state: D3D12_RESOURCE_STATES,
    pub(crate) format: DXGI_FORMAT,
    pub(crate) ready_fence: ID3D12Fence,
    pub(crate) ready_value: u64,
    pub(crate) completion_fence: ID3D12Fence,
    pub(crate) completion_value: u64,
    pub(crate) completion_submitted: bool,
}

impl D3d12Frame {
    pub(crate) fn mark_completion_submitted(&mut self) {
        self.completion_submitted = true;
    }
}

impl Drop for D3d12Frame {
    fn drop(&mut self) {
        if !self.completion_submitted {
            // No GPU consumer took ownership (for example the bounded channel
            // was full), so release the compositor cache immediately.
            let _ = unsafe { self.completion_fence.Signal(self.completion_value) };
        }
    }
}

/// Publish targets and their capture connections. Tracks are created for
/// every configured target index before signaling starts: a WHEP session can
/// only ever send media on tracks present in its answer, so a track that
/// appears after a session connects would leave that session silent forever
/// (webrtc-rs cannot late-attach a track to a negotiated trackless
/// transceiver without renegotiation, which WHEP has no channel for).
#[derive(Default)]
struct TrackTable {
    /// One long-lived track per `--webrtc` target index.
    tracks: HashMap<u32, Arc<TrackLocalStaticSample>>,
    /// Live capture connection -> target index. Streams come and go (window
    /// recreation, backend preemption) while the track persists.
    connections: HashMap<u64, u32>,
}

type Tracks = Arc<Mutex<TrackTable>>;
type Sessions = Arc<Mutex<HashMap<u64, Arc<RTCPeerConnection>>>>;
type SharedStats = Arc<Mutex<RuntimeStats>>;

const STATS_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy)]
struct Encoding {
    backend: encoders::Backend,
    codec: encoders::Codec,
}

struct RuntimeStats {
    connected_clients: HashSet<u64>,
    encodings: HashMap<u64, Encoding>,
    interval_started: Instant,
    interval_bytes: u64,
    interval_encode_time: Duration,
    interval_frames: u64,
    bitrate_bps: f64,
    encode_latency_ms: f64,
}

impl Default for RuntimeStats {
    fn default() -> Self {
        Self {
            connected_clients: HashSet::new(),
            encodings: HashMap::new(),
            interval_started: Instant::now(),
            interval_bytes: 0,
            interval_encode_time: Duration::ZERO,
            interval_frames: 0,
            bitrate_bps: 0.0,
            encode_latency_ms: 0.0,
        }
    }
}

impl RuntimeStats {
    fn record_encode(
        &mut self,
        connection: u64,
        encoded_bytes: usize,
        encode_time: Duration,
        backend: encoders::Backend,
        codec: encoders::Codec,
    ) {
        self.encodings
            .insert(connection, Encoding { backend, codec });
        self.interval_bytes = self.interval_bytes.saturating_add(encoded_bytes as u64);
        self.interval_encode_time = self.interval_encode_time.saturating_add(encode_time);
        self.interval_frames = self.interval_frames.saturating_add(1);

        self.refresh();
    }

    fn refresh(&mut self) {
        let elapsed = self.interval_started.elapsed();
        if elapsed >= STATS_INTERVAL {
            self.bitrate_bps = self.interval_bytes as f64 * 8.0 / elapsed.as_secs_f64();
            self.encode_latency_ms = if self.interval_frames == 0 {
                0.0
            } else {
                self.interval_encode_time.as_secs_f64() * 1_000.0 / self.interval_frames as f64
            };
            self.interval_started = Instant::now();
            self.interval_bytes = 0;
            self.interval_encode_time = Duration::ZERO;
            self.interval_frames = 0;
        }
    }
}

pub struct Stats {
    pub bitrate_bps: f64,
    pub encode_latency_ms: f64,
    pub connected_clients: usize,
    pub render_targets: usize,
    pub encoders: String,
    pub codecs: String,
}

pub struct Publisher {
    tracks: Tracks,
    frames: mpsc::SyncSender<VideoFrame>,
    stats: SharedStats,
    _runtime: Runtime,
}

/// Complete native WebRTC data plane for one D3D12 presentation domain.
///
/// Hosts control its lifetime, but stream registration, GPU video
/// composition, frame draining, encoding, and publication stay together so a
/// host cannot accidentally start signaling without feeding the negotiated
/// tracks.
pub struct Publication {
    publisher: Publisher,
    compositor: WebRtcCompositor,
    targets: HashSet<u32>,
    connections: HashMap<u64, u32>,
}

impl Publication {
    pub fn start_d3d12(
        bind: SocketAddr,
        targets: &[u32],
        compositor: &D3d12Compositor,
    ) -> Result<Self, Box<dyn Error>> {
        if targets.is_empty() {
            return Err("WebRTC publication requires at least one render target".into());
        }
        let requested = targets.to_vec();
        let targets = requested.iter().copied().collect::<HashSet<_>>();
        if requested.len() != targets.len() {
            return Err("WebRTC render-target indices must be unique".into());
        }
        Ok(Self {
            publisher: Publisher::start(bind, &requested)?,
            compositor: WebRtcCompositor::new_d3d12(compositor)?,
            targets,
            connections: HashMap::new(),
        })
    }

    pub fn targets(&self) -> &HashSet<u32> {
        &self.targets
    }

    pub fn add_stream(
        &mut self,
        connection: u64,
        target_index: u32,
    ) -> Result<bool, Box<dyn Error>> {
        if !self.targets.contains(&target_index) {
            return Ok(false);
        }
        self.compositor.import_d3d12_stream(connection)?;
        self.publisher.add_stream(connection, target_index);
        self.connections.insert(connection, target_index);
        Ok(true)
    }

    pub fn remove_stream(&mut self, connection: u64) {
        self.connections.remove(&connection);
        self.compositor.disconnect_stream(connection);
        self.publisher.remove_stream(connection);
    }

    /// Rebuilds only the GPU half after a presentation-adapter change. The
    /// signaling server, peer sessions, and negotiated tracks remain alive.
    pub fn reattach_d3d12(&mut self, compositor: &D3d12Compositor) -> Result<(), Box<dyn Error>> {
        let mut replacement = WebRtcCompositor::new_d3d12(compositor)?;
        for &connection in self.connections.keys() {
            replacement.import_d3d12_stream(connection)?;
        }
        self.compositor = replacement;
        Ok(())
    }

    pub fn present_d3d12(
        &mut self,
        compositor: &mut D3d12Compositor,
        plan: &CompositingPlan,
    ) -> Result<bool, Box<dyn Error>> {
        let result = compositor.present_with_webrtc(plan, Some(&mut self.compositor));
        for frame in self.compositor.take_frames() {
            self.publisher.publish(frame);
        }
        result
    }

    pub fn stats(&self) -> Stats {
        self.publisher.stats()
    }
}

impl Publisher {
    pub fn start(bind: SocketAddr, targets: &[u32]) -> Result<Self, Box<dyn Error>> {
        let listener = TcpListener::bind(bind)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("snowglass-webrtc")
            .build()?;
        let api = Arc::new(runtime.block_on(build_api(bind))?);
        let tracks = Tracks::default();
        {
            let mut table = tracks.lock();
            for &target_index in targets {
                table.tracks.insert(
                    target_index,
                    Arc::new(TrackLocalStaticSample::new(
                        hevc_capability(),
                        format!("render-target-{target_index}"),
                        "snowglass".to_owned(),
                    )),
                );
            }
        }
        let sessions = Sessions::default();
        let stats = SharedStats::default();
        let (frames, frame_rx) = mpsc::sync_channel(FRAME_CHANNEL_CAPACITY);

        spawn_encoder(
            runtime.handle().clone(),
            tracks.clone(),
            sessions.clone(),
            stats.clone(),
            frame_rx,
        );
        spawn_signaling(
            listener,
            runtime.handle().clone(),
            api,
            tracks.clone(),
            sessions,
            stats.clone(),
        );
        tracing::info!("snowglass: WebRTC signaling and ICE listening on {bind}");
        Ok(Self {
            tracks,
            frames,
            stats,
            _runtime: runtime,
        })
    }

    pub fn add_stream(&self, connection: u64, target_index: u32) {
        self.tracks
            .lock()
            .connections
            .insert(connection, target_index);
    }

    pub fn remove_stream(&self, connection: u64) {
        self.tracks.lock().connections.remove(&connection);
        self.stats.lock().encodings.remove(&connection);
    }

    pub fn publish(&self, frame: VideoFrame) {
        // Never stall the render loop behind CPU encoding or a slow peer.
        let _ = self.frames.try_send(frame);
    }

    pub fn stats(&self) -> Stats {
        let mut stats = self.stats.lock();
        stats.refresh();
        let mut encoders = stats
            .encodings
            .values()
            .map(|encoding| encoding.backend.name())
            .collect::<Vec<_>>();
        encoders.sort_unstable();
        encoders.dedup();
        let mut codecs = stats
            .encodings
            .values()
            .map(|encoding| encoding.codec.name())
            .collect::<Vec<_>>();
        codecs.sort_unstable();
        codecs.dedup();
        Stats {
            bitrate_bps: stats.bitrate_bps,
            encode_latency_ms: stats.encode_latency_ms,
            connected_clients: stats.connected_clients.len(),
            render_targets: stats.encodings.len(),
            encoders: encoders.join(", "),
            codecs: codecs.join(", "),
        }
    }
}

async fn build_api(bind: SocketAddr) -> Result<API, Box<dyn Error>> {
    let mut media = MediaEngine::default();
    media.register_default_codecs()?;
    let interceptors = register_default_interceptors(Registry::new(), &mut media)?;
    let mut settings = SettingEngine::default();
    let socket = tokio::net::UdpSocket::bind(bind).await?;
    disable_udp_connection_reset(&socket)?;
    settings.set_udp_network(UDPNetwork::Muxed(UDPMuxDefault::new(UDPMuxParams::new(
        socket,
    ))));
    // Only advertise candidates matching the mux socket's address family.
    // Mixed-family host candidates on one mux make the ICE agent file
    // peer-reflexive candidates under one family and look them up under the
    // other, so its connectivity checks never succeed.
    settings.set_network_types(vec![if bind.is_ipv4() {
        NetworkType::Udp4
    } else {
        NetworkType::Udp6
    }]);
    settings.set_include_loopback_candidate(true);
    Ok(APIBuilder::new()
        .with_media_engine(media)
        .with_interceptor_registry(interceptors)
        .with_setting_engine(settings)
        .build())
}

/// Windows surfaces ICMP port-unreachable as `WSAECONNRESET` on the next UDP
/// receive; the ICE mux read loop treats that as fatal and goes deaf, killing
/// DTLS. Sends to unreachable candidates are routine, so drop those reports.
fn disable_udp_connection_reset(socket: &tokio::net::UdpSocket) -> std::io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{SIO_UDP_CONNRESET, SOCKET_ERROR, WSAIoctl};

    let mut returned = 0u32;
    let report_connreset: u32 = 0;
    let result = unsafe {
        WSAIoctl(
            socket.as_raw_socket() as usize,
            SIO_UDP_CONNRESET,
            (&raw const report_connreset).cast(),
            size_of::<u32>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
            None,
        )
    };
    if result == SOCKET_ERROR {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn h264_capability() -> RTCRtpCodecCapability {
    RTCRtpCodecCapability {
        mime_type: MIME_TYPE_H264.to_owned(),
        clock_rate: 90_000,
        channels: 0,
        sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
            .to_owned(),
        rtcp_feedback: Vec::new(),
    }
}

fn hevc_capability() -> RTCRtpCodecCapability {
    RTCRtpCodecCapability {
        mime_type: MIME_TYPE_HEVC.to_owned(),
        clock_rate: 90_000,
        channels: 0,
        sdp_fmtp_line: String::new(),
        rtcp_feedback: Vec::new(),
    }
}

fn capability(codec: encoders::Codec) -> RTCRtpCodecCapability {
    match codec {
        encoders::Codec::Hevc => hevc_capability(),
        encoders::Codec::H264 => h264_capability(),
    }
}

fn offer_supports_hevc(sdp: &str) -> bool {
    sdp.lines().any(|line| {
        line.trim_start().starts_with("a=rtpmap:")
            && line.to_ascii_lowercase().contains(" h265/90000")
    })
}

/// Swapping a stream's track object only affects future sessions; peers that
/// already added the old track keep sending from it. Rebind every live
/// sender so connected sessions follow the codec change. Sessions whose
/// negotiated codecs cannot carry the new track keep the old (now silent)
/// track and are logged.
async fn rebind_track(
    sessions: &Sessions,
    track_id: &str,
    replacement: &Arc<TrackLocalStaticSample>,
) {
    let peers = sessions
        .lock()
        .iter()
        .map(|(session_id, peer)| (*session_id, peer.clone()))
        .collect::<Vec<_>>();
    for (session_id, peer) in peers {
        for sender in peer.get_senders().await {
            let Some(track) = sender.track().await else {
                continue;
            };
            if track.id() != track_id {
                continue;
            }
            if let Err(error) = sender
                .replace_track(Some(
                    replacement.clone() as Arc<dyn TrackLocal + Send + Sync>
                ))
                .await
            {
                tracing::error!(
                    "snowglass: WebRTC session {session_id} cannot switch to {}: {error}",
                    replacement.codec().mime_type
                );
            }
        }
    }
}

fn spawn_encoder(
    handle: Handle,
    tracks: Tracks,
    sessions: Sessions,
    stats: SharedStats,
    frames: mpsc::Receiver<VideoFrame>,
) {
    thread::Builder::new()
        .name("snowglass-video-encode".to_owned())
        .spawn(move || {
            let mut encoders = HashMap::<u64, encoders::Encoder>::new();
            let mut announced_crops = HashSet::new();
            let mut announced_samples = HashSet::new();
            let mut bitstream_dump = std::env::var_os("SNOWGLASS_DUMP_BITSTREAM")
                .and_then(|path| std::fs::File::create(path).ok());
            while let Ok(mut frame) = frames.recv() {
                let existing = {
                    let table = tracks.lock();
                    table.connections.get(&frame.connection).copied().and_then(
                        |target_index| {
                            table
                                .tracks
                                .get(&target_index)
                                .map(|track| (target_index, track.clone()))
                        },
                    )
                };
                let Some((target_index, existing_track)) = existing else { continue };
                if frame.width < 2 || frame.height < 2 {
                    continue;
                }
                let source_extent = (frame.width, frame.height);
                frame.width &= !1;
                frame.height &= !1;
                if source_extent != (frame.width, frame.height)
                    && announced_crops.insert(frame.connection)
                {
                    tracing::debug!(
                        "snowglass: cropping WebRTC track {} from {}x{} to {}x{} for 4:2:0 encoding",
                        frame.connection,
                        source_extent.0,
                        source_extent.1,
                        frame.width,
                        frame.height,
                    );
                }
                let encoder = encoders
                    .entry(frame.connection)
                    .or_insert_with(|| encoders::Encoder::new(DEFAULT_BITRATE, FRAME_RATE));
                let connection = frame.connection;
                let preferred = if existing_track
                    .codec()
                    .mime_type
                    .eq_ignore_ascii_case(MIME_TYPE_HEVC)
                {
                    encoders::Codec::Hevc
                } else {
                    encoders::Codec::H264
                };
                let encode_started = Instant::now();
                match encoder.encode(frame, preferred) {
                    Ok(encoded) => {
                        let encode_time = encode_started.elapsed();
                        stats
                            .lock()
                            .record_encode(
                                connection,
                                encoded.data.len(),
                                encode_time,
                                encoded.backend,
                                encoded.codec,
                            );
                        let wanted = capability(encoded.codec);
                        let track = if existing_track
                            .codec()
                            .mime_type
                            .eq_ignore_ascii_case(&wanted.mime_type)
                        {
                            existing_track
                        } else {
                            let replacement = Arc::new(TrackLocalStaticSample::new(
                                wanted,
                                existing_track.id().to_owned(),
                                existing_track.stream_id().to_owned(),
                            ));
                            tracks
                                .lock()
                                .tracks
                                .insert(target_index, replacement.clone());
                            tracing::info!(
                                "snowglass: WebRTC track {connection} switching to {}",
                                replacement.codec().mime_type
                            );
                            handle.block_on(rebind_track(
                                &sessions,
                                existing_track.id(),
                                &replacement,
                            ));
                            replacement
                        };
                        if announced_samples.insert(connection) {
                            tracing::debug!(
                                "snowglass: encoded first WebRTC sample for track {connection} ({} bytes)",
                                encoded.data.len()
                            );
                        }
                        if let Some(file) = bitstream_dump.as_mut() {
                            let _ = std::io::Write::write_all(file, &encoded.data);
                        }
                        let sample = Sample {
                            data: encoded.data.into(),
                            duration: FRAME_DURATION,
                            ..Default::default()
                        };
                        if let Err(error) = handle.block_on(track.write_sample(&sample)) {
                            tracing::error!("snowglass: WebRTC track write failed: {error}");
                        }
                    }
                    Err(error) => tracing::error!("snowglass: video encoding failed: {error}"),
                }
            }
        })
        .expect("failed to spawn video encoder thread");
}

fn spawn_signaling(
    listener: TcpListener,
    handle: Handle,
    api: Arc<API>,
    tracks: Tracks,
    sessions: Sessions,
    stats: SharedStats,
) {
    thread::Builder::new()
        .name("snowglass-webrtc-http".to_owned())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let handle = handle.clone();
                let api = api.clone();
                let tracks = tracks.clone();
                let sessions = sessions.clone();
                let stats = stats.clone();
                thread::spawn(move || {
                    if let Err(error) =
                        serve_http(stream, &handle, &api, &tracks, &sessions, &stats)
                    {
                        let idle_connection =
                            error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                                matches!(
                                    error.kind(),
                                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                                )
                            });
                        if !idle_connection {
                            tracing::error!("snowglass: WebRTC signaling request failed: {error}");
                        }
                    }
                });
            }
        })
        .expect("failed to spawn WebRTC signaling thread");
}

fn serve_http(
    mut stream: TcpStream,
    handle: &Handle,
    api: &API,
    tracks: &Tracks,
    sessions: &Sessions,
    stats: &SharedStats,
) -> Result<(), Box<dyn Error>> {
    let (method, path, body) = read_request(&mut stream)?;
    if method == "OPTIONS" {
        return respond_with_headers(
            &mut stream,
            "204 No Content",
            "text/plain; charset=utf-8",
            &[],
            &[("Accept-Post", "application/sdp")],
        );
    }
    if method == "DELETE" && path.starts_with("/session/") {
        let session_id = path.trim_start_matches("/session/").parse::<u64>().ok();
        if let Some(peer) = session_id.and_then(|session_id| sessions.lock().remove(&session_id)) {
            if let Some(session_id) = session_id {
                stats.lock().connected_clients.remove(&session_id);
            }
            handle.block_on(peer.close())?;
        }
        return respond(
            &mut stream,
            "204 No Content",
            "text/plain; charset=utf-8",
            &[],
        );
    }
    match (method.as_str(), path.as_str()) {
        ("GET", "/") => respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            VIEWER_HTML.as_bytes(),
        ),
        ("GET", "/tracks") => {
            let count = tracks.lock().tracks.len();
            respond(
                &mut stream,
                "200 OK",
                "application/json",
                count.to_string().as_bytes(),
            )
        }
        ("POST", "/offer") => {
            let offer: RTCSessionDescription = serde_json::from_slice(&body)?;
            let session_id = NEXT_WHEP_SESSION_ID.fetch_add(1, Ordering::Relaxed);
            let answer = handle.block_on(answer_offer(
                api, tracks, sessions, stats, session_id, offer,
            ))?;
            let json = serde_json::to_vec(&answer)?;
            respond(&mut stream, "200 OK", "application/json", &json)
        }
        ("POST", "/") => {
            let offer = RTCSessionDescription::offer(String::from_utf8(body)?)?;
            let session_id = NEXT_WHEP_SESSION_ID.fetch_add(1, Ordering::Relaxed);
            let answer = handle.block_on(answer_offer(
                api, tracks, sessions, stats, session_id, offer,
            ))?;
            let location = format!("/session/{session_id}");
            respond_with_headers(
                &mut stream,
                "201 Created",
                "application/sdp",
                answer.sdp.as_bytes(),
                &[("Location", location.as_str())],
            )
        }
        _ => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
    }
}

async fn answer_offer(
    api: &API,
    tracks: &Tracks,
    sessions: &Sessions,
    stats: &SharedStats,
    session_id: u64,
    offer: RTCSessionDescription,
) -> Result<RTCSessionDescription, Box<dyn Error>> {
    let offered_video = offer
        .sdp
        .lines()
        .any(|line| line.trim_end().starts_with("m=video "));
    if !offered_video {
        return Err("WebRTC offer contains no video media section".into());
    }
    let offered_hevc = offer_supports_hevc(&offer.sdp);
    let peer = Arc::new(api.new_peer_connection(RTCConfiguration::default()).await?);
    peer.on_ice_connection_state_change(Box::new(move |state| {
        tracing::debug!("snowglass: WebRTC session {session_id} ICE state {state}");
        Box::pin(async {})
    }));
    let weak_sessions = Arc::downgrade(sessions);
    let weak_stats = Arc::downgrade(stats);
    let weak_peer = Arc::downgrade(&peer);
    peer.on_peer_connection_state_change(Box::new(move |state| {
        let weak_sessions = weak_sessions.clone();
        let weak_stats = weak_stats.clone();
        let weak_peer = weak_peer.clone();
        Box::pin(async move {
            tracing::info!("snowglass: WebRTC session {session_id} state {state}");
            if let Some(stats) = weak_stats.upgrade() {
                let mut stats = stats.lock();
                if state == RTCPeerConnectionState::Connected {
                    stats.connected_clients.insert(session_id);
                } else {
                    stats.connected_clients.remove(&session_id);
                }
            }
            if state == RTCPeerConnectionState::Connected
                && let Some(peer) = weak_peer.upgrade()
            {
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    let (packets, bytes) = peer.get_stats().await.reports.values().fold(
                        (0_u64, 0_u64),
                        |(packets, bytes), report| match report {
                            StatsReportType::OutboundRTP(outbound) => (
                                packets.saturating_add(outbound.packets_sent),
                                bytes.saturating_add(outbound.bytes_sent),
                            ),
                            _ => (packets, bytes),
                        },
                    );
                    tracing::debug!(
                        "snowglass: WebRTC session {session_id} sent {packets} RTP packets ({bytes} payload bytes)"
                    );
                });
            }
            if matches!(
                state,
                RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
            ) && let Some(sessions) = weak_sessions.upgrade()
            {
                sessions
                    .lock()
                    .remove(&session_id);
            }
        })
    }));
    let (current_tracks, downgrades) = {
        let mut table = tracks.lock();
        let mut downgrades = Vec::new();
        if !offered_hevc {
            for track in table.tracks.values_mut() {
                if track.codec().mime_type.eq_ignore_ascii_case(MIME_TYPE_HEVC) {
                    let replacement = Arc::new(TrackLocalStaticSample::new(
                        h264_capability(),
                        track.id().to_owned(),
                        track.stream_id().to_owned(),
                    ));
                    downgrades.push((track.id().to_owned(), replacement.clone()));
                    *track = replacement;
                }
            }
        }
        // Deterministic track order so multi-target answers assign the same
        // m-line to the same render target across sessions.
        let mut entries = table.tracks.iter().collect::<Vec<_>>();
        entries.sort_by_key(|(target_index, _)| **target_index);
        (
            entries
                .into_iter()
                .map(|(_, track)| track.clone())
                .collect::<Vec<_>>(),
            downgrades,
        )
    };
    for (track_id, replacement) in downgrades {
        tracing::warn!(
            "snowglass: WebRTC session {session_id} offer has no HEVC; switching track {track_id} to H.264"
        );
        rebind_track(sessions, &track_id, &replacement).await;
    }
    for track in current_tracks {
        let sender = peer
            .add_track(track as Arc<dyn TrackLocal + Send + Sync>)
            .await?;
        tokio::spawn(async move { while sender.read_rtcp().await.is_ok() {} });
    }
    peer.set_remote_description(offer).await?;
    for transceiver in peer.get_transceivers().await {
        if transceiver.kind() == RTPCodecType::Audio {
            transceiver
                .set_direction(RTCRtpTransceiverDirection::Inactive)
                .await;
        }
    }
    let mut gathering = peer.gathering_complete_promise().await;
    peer.set_local_description(peer.create_answer(None).await?)
        .await?;
    for transceiver in peer.get_transceivers().await {
        let has_track = transceiver.sender().await.track().await.is_some();
        tracing::debug!(
            "snowglass: WebRTC session {session_id} {:?} transceiver direction={} negotiated={} track={has_track}",
            transceiver.kind(),
            transceiver.direction(),
            transceiver.current_direction(),
        );
    }
    if tokio::time::timeout(ICE_GATHERING_TIMEOUT, gathering.recv())
        .await
        .is_err()
    {
        tracing::warn!(
            "snowglass: WebRTC session {session_id} ICE gathering timed out; returning gathered candidates"
        );
    }
    let answer = peer
        .local_description()
        .await
        .ok_or("WebRTC answer has no local description")?;
    sessions.lock().insert(session_id, peer);
    Ok(answer)
}

fn read_request(stream: &mut TcpStream) -> Result<(String, String, Vec<u8>), Box<dyn Error>> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut request = Vec::new();
    let header_end;
    loop {
        let mut chunk = [0u8; 4096];
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            return Err("incomplete HTTP request".into());
        }
        request.extend_from_slice(&chunk[..count]);
        if request.len() > MAX_HTTP_HEADER {
            return Err("HTTP header is too large".into());
        }
        if let Some(position) = request.windows(4).position(|value| value == b"\r\n\r\n") {
            header_end = position + 4;
            break;
        }
    }
    let header = std::str::from_utf8(&request[..header_end])?;
    let mut lines = header.split("\r\n");
    let mut request_line = lines
        .next()
        .ok_or("missing HTTP request line")?
        .split_whitespace();
    let method = request_line.next().ok_or("missing HTTP method")?.to_owned();
    let path = request_line.next().ok_or("missing HTTP path")?.to_owned();
    let content_length = lines
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while request.len() < header_end + content_length {
        let mut chunk = [0u8; 4096];
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            return Err("incomplete HTTP body".into());
        }
        request.extend_from_slice(&chunk[..count]);
    }
    Ok((
        method,
        path,
        request[header_end..header_end + content_length].to_vec(),
    ))
}

fn respond(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> Result<(), Box<dyn Error>> {
    respond_with_headers(stream, status, content_type, body, &[])
}

fn respond_with_headers(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    headers: &[(&str, &str)],
) -> Result<(), Box<dyn Error>> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, DELETE, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nAccess-Control-Expose-Headers: Location\r\nAccess-Control-Allow-Private-Network: true\r\nAccess-Control-Max-Age: 86400\r\n",
        body.len()
    )?;
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    write!(stream, "\r\n")?;
    stream.write_all(body)?;
    Ok(())
}

const VIEWER_HTML: &str = r#"<!doctype html>
<meta charset="utf-8"><title>Snowglass WebRTC</title>
<style>body{margin:0;background:#111;color:#ddd;font:14px sans-serif;display:grid;gap:8px;padding:8px}video{max-width:100%;background:#000}</style>
<script type="module">
const count = await (await fetch('/tracks')).json();
const pc = new RTCPeerConnection();
for (let i = 0; i < count; ++i) pc.addTransceiver('video', {direction:'recvonly'});
pc.ontrack = ({track}) => { const video=document.createElement('video'); video.autoplay=true; video.playsInline=true; video.srcObject=new MediaStream([track]); document.body.append(video); };
await pc.setLocalDescription(await pc.createOffer());
await new Promise(resolve => pc.iceGatheringState === 'complete' ? resolve() : pc.addEventListener('icegatheringstatechange', () => pc.iceGatheringState === 'complete' && resolve()));
const answer = await (await fetch('/offer', {method:'POST', headers:{'content-type':'application/json'}, body:JSON.stringify(pc.localDescription)})).json();
await pc.setRemoteDescription(answer);
</script>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h264_codec_uses_browser_compatible_packetization() {
        let codec = h264_capability();
        assert_eq!(codec.mime_type, "video/H264");
        assert!(codec.sdp_fmtp_line.contains("packetization-mode=1"));
        assert!(codec.sdp_fmtp_line.contains("profile-level-id=42e01f"));
        assert_eq!(codec.clock_rate, 90_000);
    }

    #[test]
    fn hevc_codec_uses_the_webrtc_clock_rate() {
        let codec = hevc_capability();
        assert_eq!(codec.mime_type, "video/H265");
        assert_eq!(codec.clock_rate, 90_000);
    }

    #[test]
    fn codec_selection_falls_back_when_offer_omits_hevc() {
        assert!(offer_supports_hevc("a=rtpmap:126 H265/90000\r\n"));
        assert!(!offer_supports_hevc("a=rtpmap:102 H264/90000\r\n"));
    }

    #[test]
    fn encode_statistics_measure_payload_rate_and_latency() {
        let mut stats = RuntimeStats {
            interval_started: Instant::now() - Duration::from_secs(1),
            ..Default::default()
        };
        stats.record_encode(
            7,
            125_000,
            Duration::from_millis(4),
            encoders::Backend::D3d12,
            encoders::Codec::Hevc,
        );

        assert!((stats.bitrate_bps - 1_000_000.0).abs() < 10_000.0);
        assert_eq!(stats.encode_latency_ms, 4.0);
        assert_eq!(stats.encodings.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires local UDP loopback ICE"]
    async fn whep_session_retains_peer_and_sends_rtp() -> Result<(), Box<dyn Error>> {
        use webrtc::{
            api::media_engine::MediaEngine,
            rtp_transceiver::{
                RTCRtpTransceiverInit, rtp_codec::RTPCodecType,
                rtp_transceiver_direction::RTCRtpTransceiverDirection,
            },
        };

        let listener = TcpListener::bind("127.0.0.1:0")?;
        let server_address = listener.local_addr()?;
        let tracks = Tracks::default();
        let server_track = Arc::new(TrackLocalStaticSample::new(
            h264_capability(),
            "test-video".to_owned(),
            "snowglass-test".to_owned(),
        ));
        {
            let mut table = tracks.lock();
            table.tracks.insert(0, server_track.clone());
            table.connections.insert(1, 0);
        }
        let server_api = Arc::new(build_api(server_address).await?);
        spawn_signaling(
            listener,
            Handle::current(),
            server_api,
            tracks,
            Sessions::default(),
            SharedStats::default(),
        );

        let mut media = MediaEngine::default();
        media.register_default_codecs()?;
        let interceptors = register_default_interceptors(Registry::new(), &mut media)?;
        let mut client_settings = SettingEngine::default();
        client_settings.set_include_loopback_candidate(true);
        let peer = Arc::new(
            APIBuilder::new()
                .with_media_engine(media)
                .with_interceptor_registry(interceptors)
                .with_setting_engine(client_settings)
                .build()
                .new_peer_connection(RTCConfiguration::default())
                .await?,
        );
        peer.add_transceiver_from_kind(
            RTPCodecType::Video,
            Some(RTCRtpTransceiverInit {
                direction: RTCRtpTransceiverDirection::Recvonly,
                send_encodings: Vec::new(),
            }),
        )
        .await?;
        let (packet_tx, mut packet_rx) = tokio::sync::mpsc::channel(1);
        peer.on_track(Box::new(move |track, _, _| {
            let packet_tx = packet_tx.clone();
            Box::pin(async move {
                if let Ok((packet, _)) = track.read_rtp().await {
                    let _ = packet_tx.send(packet.payload.len()).await;
                }
            })
        }));
        let mut gathering = peer.gathering_complete_promise().await;
        peer.set_local_description(peer.create_offer(None).await?)
            .await?;
        let _ = gathering.recv().await;
        let offer = peer.local_description().await.ok_or("missing offer")?.sdp;
        let answer = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let mut stream = TcpStream::connect(server_address).map_err(|e| e.to_string())?;
            write!(
                stream,
                "POST / HTTP/1.1\r\nHost: {server_address}\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                offer.len(),
                offer
            )
            .map_err(|e| e.to_string())?;
            let mut response = String::new();
            stream
                .read_to_string(&mut response)
                .map_err(|e| e.to_string())?;
            let (header, body) = response
                .split_once("\r\n\r\n")
                .ok_or_else(|| "malformed HTTP response".to_owned())?;
            if !header.starts_with("HTTP/1.1 201") {
                return Err(header.to_owned());
            }
            Ok(body.to_owned())
        })
        .await??;
        peer.set_remote_description(RTCSessionDescription::answer(answer)?)
            .await?;
        let writer = tokio::spawn(async move {
            loop {
                let _ = server_track
                    .write_sample(&Sample {
                        data: vec![0, 0, 0, 1, 0x65, 0x88, 0x84].into(),
                        duration: FRAME_DURATION,
                        ..Default::default()
                    })
                    .await;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        let payload_len =
            match tokio::time::timeout(Duration::from_secs(10), packet_rx.recv()).await {
                Ok(Some(payload_len)) => payload_len,
                Ok(None) => return Err("RTP receiver closed".into()),
                Err(_) => {
                    return Err(format!(
                        "timed out waiting for RTP (peer={:?}, ice={:?}, signaling={:?})",
                        peer.connection_state(),
                        peer.ice_connection_state(),
                        peer.signaling_state()
                    )
                    .into());
                }
            };
        assert!(payload_len > 0);
        writer.abort();
        peer.close().await?;
        Ok(())
    }
}
