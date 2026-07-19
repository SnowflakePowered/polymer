//! Headless WHEP client used to exercise the viewer's WebRTC path.
//! Usage: whep-probe <http://host:port/> [seconds] [h264]
//! Passing `h264` offers only H.264 (no HEVC), like a browser without
//! hardware HEVC decoding.
#![allow(clippy::print_stderr, clippy::print_stdout)]

use std::{
    error::Error,
    io::{Read, Write},
    net::TcpStream,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use webrtc::{
    api::{
        APIBuilder,
        interceptor_registry::register_default_interceptors,
        media_engine::{MIME_TYPE_H264, MediaEngine},
        setting_engine::SettingEngine,
    },
    interceptor::registry::Registry,
    peer_connection::{
        configuration::RTCConfiguration, sdp::session_description::RTCSessionDescription,
    },
    rtp_transceiver::{
        RTCRtpTransceiverInit,
        rtp_codec::{RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType},
        rtp_transceiver_direction::RTCRtpTransceiverDirection,
    },
};

/// RFC 7798: single NAL passes through; AP (48) carries length-prefixed NALs;
/// FU (49) reassembles fragments into `fragment`.
fn depacketize_h265(payload: &[u8], fragment: &mut Vec<u8>) -> Vec<Vec<u8>> {
    if payload.len() < 2 {
        return Vec::new();
    }
    let nal_type = (payload[0] >> 1) & 0x3f;
    match nal_type {
        48 => {
            let mut nals = Vec::new();
            let mut index = 2;
            while index + 2 <= payload.len() {
                let len = u16::from_be_bytes([payload[index], payload[index + 1]]) as usize;
                index += 2;
                if index + len > payload.len() {
                    break;
                }
                nals.push(payload[index..index + len].to_vec());
                index += len;
            }
            nals
        }
        49 => {
            if payload.len() < 3 {
                return Vec::new();
            }
            let fu_header = payload[2];
            let start = fu_header & 0x80 != 0;
            let end = fu_header & 0x40 != 0;
            if start {
                fragment.clear();
                let original_type = fu_header & 0x3f;
                fragment.push((payload[0] & 0x81) | (original_type << 1));
                fragment.push(payload[1]);
            }
            fragment.extend_from_slice(&payload[3..]);
            if end {
                return vec![std::mem::take(fragment)];
            }
            Vec::new()
        }
        _ => vec![payload.to_vec()],
    }
}

/// RFC 6184: single NAL passes through; STAP-A (24) carries length-prefixed
/// NALs; FU-A (28) reassembles fragments.
fn depacketize_h264(payload: &[u8], fragment: &mut Vec<u8>) -> Vec<Vec<u8>> {
    if payload.is_empty() {
        return Vec::new();
    }
    match payload[0] & 0x1f {
        24 => {
            let mut nals = Vec::new();
            let mut index = 1;
            while index + 2 <= payload.len() {
                let len = u16::from_be_bytes([payload[index], payload[index + 1]]) as usize;
                index += 2;
                if index + len > payload.len() {
                    break;
                }
                nals.push(payload[index..index + len].to_vec());
                index += len;
            }
            nals
        }
        28 => {
            if payload.len() < 2 {
                return Vec::new();
            }
            let fu_header = payload[1];
            if fu_header & 0x80 != 0 {
                fragment.clear();
                fragment.push((payload[0] & 0xe0) | (fu_header & 0x1f));
            }
            fragment.extend_from_slice(&payload[2..]);
            if fu_header & 0x40 != 0 {
                return vec![std::mem::take(fragment)];
            }
            Vec::new()
        }
        _ => vec![payload.to_vec()],
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let url = args.next().unwrap_or("http://127.0.0.1:9001/".to_owned());
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(10);
    let h264_only = args.next().is_some_and(|mode| mode == "h264");
    let address = url
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned();

    let mut media = MediaEngine::default();
    if h264_only {
        media.register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: MIME_TYPE_H264.to_owned(),
                    clock_rate: 90_000,
                    channels: 0,
                    sdp_fmtp_line:
                        "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                            .to_owned(),
                    rtcp_feedback: Vec::new(),
                },
                payload_type: 102,
                ..Default::default()
            },
            RTPCodecType::Video,
        )?;
    } else {
        media.register_default_codecs()?;
    }
    let interceptors = register_default_interceptors(Registry::new(), &mut media)?;
    let mut settings = SettingEngine::default();
    settings.set_include_loopback_candidate(true);
    let peer = Arc::new(
        APIBuilder::new()
            .with_media_engine(media)
            .with_interceptor_registry(interceptors)
            .with_setting_engine(settings)
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

    let packets = Arc::new(AtomicU64::new(0));
    let bytes = Arc::new(AtomicU64::new(0));
    let dump_path = std::env::var_os("WHEP_PROBE_DUMP").map(std::path::PathBuf::from);
    {
        let packets = packets.clone();
        let bytes = bytes.clone();
        peer.on_track(Box::new(move |track, _, _| {
            let packets = packets.clone();
            let bytes = bytes.clone();
            let dump_path = dump_path.clone();
            let mime = track.codec().capability.mime_type;
            eprintln!("probe: on_track kind={} codec={mime}", track.kind());
            Box::pin(async move {
                let mut dump = dump_path
                    .and_then(|path| std::fs::File::create(path).ok())
                    .map(|file| (file, Vec::new()));
                let hevc = mime.eq_ignore_ascii_case("video/H265");
                while let Ok((packet, _)) = track.read_rtp().await {
                    packets.fetch_add(1, Ordering::Relaxed);
                    bytes.fetch_add(packet.payload.len() as u64, Ordering::Relaxed);
                    if let Some((file, fragment)) = dump.as_mut() {
                        let nals = if hevc {
                            depacketize_h265(&packet.payload, fragment)
                        } else {
                            depacketize_h264(&packet.payload, fragment)
                        };
                        for nal in nals {
                            use std::io::Write;
                            let _ = file.write_all(&[0, 0, 0, 1]);
                            let _ = file.write_all(&nal);
                        }
                    }
                }
            })
        }));
    }
    peer.on_peer_connection_state_change(Box::new(move |state| {
        eprintln!("probe: peer state {state}");
        Box::pin(async {})
    }));
    peer.on_ice_connection_state_change(Box::new(move |state| {
        eprintln!("probe: ice state {state}");
        Box::pin(async {})
    }));

    let mut gathering = peer.gathering_complete_promise().await;
    peer.set_local_description(peer.create_offer(None).await?)
        .await?;
    let _ = gathering.recv().await;
    let offer = peer.local_description().await.ok_or("no local offer")?.sdp;
    eprintln!("probe: sending offer ({} bytes) to {address}", offer.len());

    let (status, answer) = tokio::task::spawn_blocking({
        let address = address.clone();
        move || -> Result<(String, String), String> {
            let mut stream = TcpStream::connect(&address).map_err(|e| e.to_string())?;
            write!(
                stream,
                "POST / HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
            Ok((
                header.lines().next().unwrap_or("").to_owned(),
                body.to_owned(),
            ))
        }
    })
    .await??;
    eprintln!("probe: signaling response: {status}");
    if !status.contains("201") {
        return Err(format!("unexpected signaling status: {status}\n{answer}").into());
    }
    peer.set_remote_description(RTCSessionDescription::answer(answer)?)
        .await?;

    for second in 1..=seconds {
        tokio::time::sleep(Duration::from_secs(1)).await;
        eprintln!(
            "probe: t={second}s packets={} payload_bytes={} state={}",
            packets.load(Ordering::Relaxed),
            bytes.load(Ordering::Relaxed),
            peer.connection_state()
        );
    }
    let received = packets.load(Ordering::Relaxed);
    peer.close().await?;
    if received == 0 {
        eprintln!("probe: FAILED - no RTP received");
        std::process::exit(2);
    }
    eprintln!("probe: OK - received {received} RTP packets");
    Ok(())
}
