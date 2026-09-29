//! WebRTC signaling and one-track-per-capture-stream publishing.
#![allow(unsafe_code)]

use axum::{
    Router,
    body::Bytes,
    extract::{Path, Request, State},
    http::{
        HeaderName, HeaderValue, Method, StatusCode,
        header::{
            ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS,
            ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_EXPOSE_HEADERS, ACCESS_CONTROL_MAX_AGE,
            CONTENT_TYPE, LOCATION,
        },
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use parking_lot::Mutex;
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    net::{SocketAddr, TcpListener},
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
const FRAME_RATE: f32 = 60.0;
const FRAME_DURATION: Duration = Duration::from_micros(16_667);
const DEFAULT_BITRATE: u32 = 8_000_000;
/// Bounded frame handoff to the encoder thread. GPU-backed frame producers
/// must never have more frames outstanding than this, so an accepted frame
/// can never resolve after a later frame that was dropped by `try_send`.
pub const FRAME_CHANNEL_CAPACITY: usize = 2;
const ICE_GATHERING_TIMEOUT: Duration = Duration::from_secs(1);
static NEXT_WHEP_SESSION_ID: AtomicU64 = AtomicU64::new(1);

mod frame;
mod session;
pub use frame::{Codec, EncodeError, EncodedFrame, VideoFrame, VideoSwapchain};
pub use session::{EncoderSession, NativeEncoder};

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
    backend: &'static str,
    codec: Codec,
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
        backend: &'static str,
        codec: Codec,
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

enum EncodeCommand {
    Frame(VideoFrame),
    Flush(mpsc::Sender<()>),
}

pub struct Publisher {
    tracks: Tracks,
    frames: mpsc::SyncSender<EncodeCommand>,
    stats: SharedStats,
    _runtime: Runtime,
}

impl Publisher {
    pub fn start(bind: SocketAddr, targets: &[u32]) -> Result<Self, Box<dyn Error>> {
        let listener = TcpListener::bind(bind)?;
        listener.set_nonblocking(true)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("polymer-webrtc")
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
                        "polymer".to_owned(),
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
        Signaling {
            api,
            tracks: tracks.clone(),
            sessions,
            stats: stats.clone(),
        }
        .spawn(&runtime, listener)?;
        tracing::info!("polymer: WebRTC signaling and ICE listening on {bind}");
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
        // Never stall the render loop behind hardware encoding or a slow peer.
        let _ = self.frames.try_send(EncodeCommand::Frame(frame));
    }

    /// Finish previously submitted jobs before a backend destroys borrowed GPU resources.
    pub fn flush(&self) {
        let (done, completed) = mpsc::channel();
        if self.frames.send(EncodeCommand::Flush(done)).is_ok() {
            let _ = completed.recv();
        }
    }

    pub fn stats(&self) -> Stats {
        let mut stats = self.stats.lock();
        stats.refresh();
        let mut encoders = stats
            .encodings
            .values()
            .map(|encoding| encoding.backend)
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
    #[cfg(windows)]
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
#[cfg(windows)]
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

fn capability(codec: Codec) -> RTCRtpCodecCapability {
    match codec {
        Codec::Hevc => hevc_capability(),
        Codec::H264 => h264_capability(),
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
                    "polymer: WebRTC session {session_id} cannot switch to {}: {error}",
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
    frames: mpsc::Receiver<EncodeCommand>,
) {
    thread::Builder::new()
        .name("polymer-video-encode".to_owned())
        .spawn(move || {
            let mut announced_crops = HashSet::new();
            let mut announced_samples = HashSet::new();
            let mut bitstream_dump = std::env::var_os("POLYMER_DUMP_BITSTREAM")
                .and_then(|path| std::fs::File::create(path).ok());
            while let Ok(command) = frames.recv() {
                let mut frame = match command {
                    EncodeCommand::Frame(frame) => frame,
                    EncodeCommand::Flush(done) => {
                        let _ = done.send(());
                        continue;
                    }
                };
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
                        "polymer: cropping WebRTC track {} from {}x{} to {}x{} for 4:2:0 encoding",
                        frame.connection,
                        source_extent.0,
                        source_extent.1,
                        frame.width,
                        frame.height,
                    );
                }
                let connection = frame.connection;
                let preferred = if existing_track
                    .codec()
                    .mime_type
                    .eq_ignore_ascii_case(MIME_TYPE_HEVC)
                {
                    Codec::Hevc
                } else {
                    Codec::H264
                };
                let encode_started = Instant::now();
                match frame.encode(preferred, DEFAULT_BITRATE, FRAME_RATE) {
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
                                "polymer: WebRTC track {connection} switching to {}",
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
                                "polymer: encoded first WebRTC sample for track {connection} ({} bytes)",
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
                            tracing::error!("polymer: WebRTC track write failed: {error}");
                        }
                    }
                    Err(error) => tracing::error!("polymer: video encoding failed: {error}"),
                }
            }
        })
        .expect("failed to spawn video encoder thread");
}

#[derive(Clone)]
struct Signaling {
    api: Arc<API>,
    tracks: Tracks,
    sessions: Sessions,
    stats: SharedStats,
}

impl Signaling {
    fn spawn(self, runtime: &Runtime, listener: TcpListener) -> std::io::Result<()> {
        let listener = {
            let _context = runtime.enter();
            tokio::net::TcpListener::from_std(listener)?
        };
        let router = Router::new()
            .route("/", get(Self::viewer).post(Self::whep))
            .route("/tracks", get(Self::tracks))
            .route("/offer", post(Self::offer))
            .route("/session/{id}", delete(Self::close))
            .fallback(Self::not_found)
            .method_not_allowed_fallback(Self::not_found)
            .layer(middleware::from_fn(Self::cors))
            .with_state(self);
        runtime.spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                tracing::error!("polymer: WebRTC signaling server failed: {error}");
            }
        });
        Ok(())
    }

    async fn cors(request: Request, next: Next) -> Response {
        let mut response = if request.method() == Method::OPTIONS {
            (
                StatusCode::NO_CONTENT,
                [
                    (CONTENT_TYPE, "text/plain; charset=utf-8"),
                    (HeaderName::from_static("accept-post"), "application/sdp"),
                ],
            )
                .into_response()
        } else {
            next.run(request).await
        };
        let headers = response.headers_mut();
        for (name, value) in [
            (ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, DELETE, OPTIONS"),
            (ACCESS_CONTROL_ALLOW_HEADERS, "Content-Type"),
            (ACCESS_CONTROL_EXPOSE_HEADERS, "Location"),
            (
                HeaderName::from_static("access-control-allow-private-network"),
                "true",
            ),
            (ACCESS_CONTROL_MAX_AGE, "86400"),
        ] {
            headers.insert(name, HeaderValue::from_static(value));
        }
        response
    }

    async fn viewer() -> Response {
        ([(CONTENT_TYPE, "text/html; charset=utf-8")], VIEWER_HTML).into_response()
    }

    async fn tracks(State(signaling): State<Self>) -> Response {
        let count = signaling.tracks.lock().tracks.len();
        ([(CONTENT_TYPE, "application/json")], count.to_string()).into_response()
    }

    async fn offer(State(signaling): State<Self>, body: Bytes) -> Response {
        let offer = match serde_json::from_slice(&body) {
            Ok(offer) => offer,
            Err(error) => return Self::failed(error.into()),
        };
        let session_id = NEXT_WHEP_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        let answer = answer_offer(
            &signaling.api,
            &signaling.tracks,
            &signaling.sessions,
            &signaling.stats,
            session_id,
            offer,
        )
        .await;
        match answer.and_then(|answer| Ok(serde_json::to_vec(&answer)?)) {
            Ok(json) => ([(CONTENT_TYPE, "application/json")], json).into_response(),
            Err(error) => Self::failed(error),
        }
    }

    async fn whep(State(signaling): State<Self>, body: Bytes) -> Response {
        let offer = match String::from_utf8(body.to_vec())
            .map_err(Box::<dyn Error>::from)
            .and_then(|sdp| Ok(RTCSessionDescription::offer(sdp)?))
        {
            Ok(offer) => offer,
            Err(error) => return Self::failed(error),
        };
        let session_id = NEXT_WHEP_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        match answer_offer(
            &signaling.api,
            &signaling.tracks,
            &signaling.sessions,
            &signaling.stats,
            session_id,
            offer,
        )
        .await
        {
            Ok(answer) => (
                StatusCode::CREATED,
                [
                    (CONTENT_TYPE, "application/sdp".to_owned()),
                    (LOCATION, format!("/session/{session_id}")),
                ],
                answer.sdp,
            )
                .into_response(),
            Err(error) => Self::failed(error),
        }
    }

    async fn close(State(signaling): State<Self>, Path(id): Path<String>) -> Response {
        let session_id = id.parse::<u64>().ok();
        let peer = session_id.and_then(|session_id| signaling.sessions.lock().remove(&session_id));
        if let (Some(session_id), Some(peer)) = (session_id, peer) {
            signaling.stats.lock().connected_clients.remove(&session_id);
            if let Err(error) = peer.close().await {
                return Self::failed(error.into());
            }
        }
        (
            StatusCode::NO_CONTENT,
            [(CONTENT_TYPE, "text/plain; charset=utf-8")],
        )
            .into_response()
    }

    async fn not_found() -> Response {
        (
            StatusCode::NOT_FOUND,
            [(CONTENT_TYPE, "text/plain")],
            "not found",
        )
            .into_response()
    }

    fn failed(error: Box<dyn Error>) -> Response {
        tracing::error!("polymer: WebRTC signaling request failed: {error}");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
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
        tracing::debug!("polymer: WebRTC session {session_id} ICE state {state}");
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
            tracing::info!("polymer: WebRTC session {session_id} state {state}");
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
                        "polymer: WebRTC session {session_id} sent {packets} RTP packets ({bytes} payload bytes)"
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
            "polymer: WebRTC session {session_id} offer has no HEVC; switching track {track_id} to H.264"
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
            "polymer: WebRTC session {session_id} {:?} transceiver direction={} negotiated={} track={has_track}",
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
            "polymer: WebRTC session {session_id} ICE gathering timed out; returning gathered candidates"
        );
    }
    let answer = peer
        .local_description()
        .await
        .ok_or("WebRTC answer has no local description")?;
    sessions.lock().insert(session_id, peer);
    Ok(answer)
}

const VIEWER_HTML: &str = r#"<!doctype html>
<meta charset="utf-8"><title>Polymer WebRTC</title>
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
