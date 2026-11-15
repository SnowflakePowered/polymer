use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    time,
};

use crate::{
    Error, Result,
    transport::bredr::hci::{HciHost, L2capFrames},
};

use super::{
    ConfigurationOption, DEFAULT_MTU, HID_CONTROL_PSM, HID_INTERRUPT_PSM, Packet, SIGNALING_CID,
    Signal, packet::FIRST_DYNAMIC_CID,
};

const OPEN_CHANNEL_TIMEOUT: Duration = Duration::from_secs(8);
const ACCEPT_CHANNEL_TIMEOUT: Duration = Duration::from_secs(30);

enum LinkCommand {
    Open {
        psm: u16,
        response: oneshot::Sender<Result<Channel>>,
    },
    Accept {
        psm: u16,
        response: oneshot::Sender<Result<Channel>>,
    },
    QueryExtendedFeatures {
        response: oneshot::Sender<Result<()>>,
    },
    Send {
        remote_cid: u16,
        payload: Bytes,
        flushable: bool,
        response: oneshot::Sender<Result<()>>,
    },
    Close {
        local_cid: u16,
        remote_cid: u16,
    },
}

struct PendingChannel {
    psm: u16,
    local_cid: u16,
    remote_cid: u16,
    local_configured: bool,
    remote_configured: bool,
    response: oneshot::Sender<Result<Channel>>,
}

struct OpenChannel {
    remote_cid: u16,
    sender: mpsc::Sender<Bytes>,
}

struct StoredRequest {
    identifier: u8,
    psm: u16,
    remote_cid: u16,
}

#[derive(Clone)]
pub struct Link {
    commands: mpsc::Sender<LinkCommand>,
}

impl Link {
    pub fn new(host: Arc<HciHost>, handle: u16) -> Self {
        let frames = host.l2cap_frames();
        Self::new_with_frame_receiver(host, handle, frames)
    }

    pub fn new_with_frame_receiver(
        host: Arc<HciHost>,
        handle: u16,
        frames: L2capFrames,
    ) -> Self {
        let (commands, receiver) = mpsc::channel(64);
        tokio::spawn(link_actor(
            host,
            handle & 0x0fff,
            frames,
            commands.downgrade(),
            receiver,
        ));
        Self { commands }
    }

    pub async fn open_channel(&self, psm: u16) -> Result<Channel> {
        self.request_channel(LinkRequest::Open, psm).await
    }

    /// Announces the host's supported extended L2CAP features.
    ///
    /// This mirrors the information exchange performed by established Switch
    /// controller hosts before completing the device-initiated HID channels.
    pub async fn query_extended_features(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(LinkCommand::QueryExtendedFeatures { response: tx })
            .await
            .map_err(|_| Error::ChannelClosed)?;
        rx.await.map_err(|_| Error::ChannelClosed)?
    }

    pub async fn accept_channel(&self, psm: u16) -> Result<Channel> {
        self.request_channel(LinkRequest::Accept, psm).await
    }

    async fn request_channel(&self, request: LinkRequest, psm: u16) -> Result<Channel> {
        let (tx, rx) = oneshot::channel();
        let timeout = match request {
            LinkRequest::Open => OPEN_CHANNEL_TIMEOUT,
            LinkRequest::Accept => ACCEPT_CHANNEL_TIMEOUT,
        };
        let command = match request {
            LinkRequest::Open => LinkCommand::Open { psm, response: tx },
            LinkRequest::Accept => LinkCommand::Accept { psm, response: tx },
        };
        self.commands
            .send(command)
            .await
            .map_err(|_| Error::ChannelClosed)?;
        time::timeout(timeout, rx)
            .await
            .map_err(|_| Error::Timeout("L2CAP channel configuration"))?
            .map_err(|_| Error::ChannelClosed)?
    }
}

#[derive(Clone, Copy)]
enum LinkRequest {
    Open,
    Accept,
}

pub struct Channel {
    local_cid: u16,
    remote_cid: u16,
    psm: u16,
    commands: mpsc::Sender<LinkCommand>,
    receiver: mpsc::Receiver<Bytes>,
}

#[derive(Clone)]
pub struct ChannelSender {
    remote_cid: u16,
    commands: mpsc::Sender<LinkCommand>,
}

impl Channel {
    pub fn psm(&self) -> u16 {
        self.psm
    }
    pub fn local_cid(&self) -> u16 {
        self.local_cid
    }
    pub fn remote_cid(&self) -> u16 {
        self.remote_cid
    }

    pub fn sender(&self) -> ChannelSender {
        ChannelSender {
            remote_cid: self.remote_cid,
            commands: self.commands.clone(),
        }
    }

    pub async fn send(&self, payload: impl Into<Bytes>, flushable: bool) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(LinkCommand::Send {
                remote_cid: self.remote_cid,
                payload: payload.into(),
                flushable,
                response: tx,
            })
            .await
            .map_err(|_| Error::ChannelClosed)?;
        rx.await.map_err(|_| Error::ChannelClosed)?
    }

    pub async fn receive(&mut self) -> Result<Bytes> {
        self.receiver.recv().await.ok_or(Error::ChannelClosed)
    }
}

impl ChannelSender {
    pub async fn send(&self, payload: impl Into<Bytes>, flushable: bool) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(LinkCommand::Send {
                remote_cid: self.remote_cid,
                payload: payload.into(),
                flushable,
                response: tx,
            })
            .await
            .map_err(|_| Error::ChannelClosed)?;
        rx.await.map_err(|_| Error::ChannelClosed)?
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        let _ = self.commands.try_send(LinkCommand::Close {
            local_cid: self.local_cid,
            remote_cid: self.remote_cid,
        });
    }
}

async fn link_actor(
    host: Arc<HciHost>,
    handle: u16,
    mut frames: L2capFrames,
    command_sender: mpsc::WeakSender<LinkCommand>,
    mut commands: mpsc::Receiver<LinkCommand>,
) {
    let mut next_cid = FIRST_DYNAMIC_CID;
    let mut next_identifier = 1u8;
    let mut pending: HashMap<u16, PendingChannel> = HashMap::new();
    let mut connection_ids: HashMap<u8, u16> = HashMap::new();
    let mut configuration_ids: HashMap<u8, u16> = HashMap::new();
    let mut accepts: HashMap<u16, VecDeque<oneshot::Sender<Result<Channel>>>> = HashMap::new();
    let mut stored_requests: VecDeque<StoredRequest> = VecDeque::new();
    let mut channels: HashMap<u16, OpenChannel> = HashMap::new();

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { break; };
                discard_abandoned(&host, handle, &mut next_identifier, &mut pending, &mut accepts, &mut connection_ids, &mut configuration_ids).await;
                match command {
                    LinkCommand::Open { psm, response } => {
                        if let Some(index) = stored_requests.iter().position(|request| request.psm == psm) {
                            let request = stored_requests.remove(index).unwrap();
                            accept_request(request, response, &host, handle, &mut next_cid, &mut next_identifier, &mut pending, &channels).await;
                        } else {
                            let local_cid = allocate_cid_for_psm(psm, &mut next_cid, &pending, &channels);
                            let identifier = allocate_identifier(&mut next_identifier);
                            pending.insert(local_cid, PendingChannel { psm, local_cid, remote_cid: 0, local_configured: false, remote_configured: false, response });
                            connection_ids.insert(identifier, local_cid);
                            let signal = Signal::ConnectionRequest { identifier, psm, source_cid: local_cid };
                            if let Err(error) = send_signal(&host, handle, signal).await { fail_pending(local_cid, error, &mut pending); }
                        }
                    }
                    LinkCommand::Accept { psm, response } => {
                        if let Some(index) = stored_requests.iter().position(|request| request.psm == psm) {
                            let request = stored_requests.remove(index).unwrap();
                            accept_request(request, response, &host, handle, &mut next_cid, &mut next_identifier, &mut pending, &channels).await;
                        } else {
                            accepts.entry(psm).or_default().push_back(response);
                        }
                    }
                    LinkCommand::QueryExtendedFeatures { response } => {
                        let identifier = allocate_identifier(&mut next_identifier);
                        let result = send_signal(
                            &host,
                            handle,
                            Signal::InformationRequest {
                                identifier,
                                info_type: 0x0002,
                            },
                        )
                        .await;
                        let _ = response.send(result);
                    }
                    LinkCommand::Send { remote_cid, payload, flushable, response } => {
                        let result = host.send_l2cap(handle, &Packet::new(remote_cid, payload).encode(), flushable).await;
                        let _ = response.send(result);
                    }
                    LinkCommand::Close { local_cid, remote_cid } => {
                        channels.remove(&local_cid);
                        let identifier = allocate_identifier(&mut next_identifier);
                        let _ = send_signal(&host, handle, Signal::DisconnectionRequest { identifier, destination_cid: remote_cid, source_cid: local_cid }).await;
                    }
                }
            }
            frame = async {
                tokio::select! {
                    biased;
                    frame = frames.signals.recv() => frame,
                    frame = frames.data.recv() => frame,
                }
            } => {
                let frame = match frame {
                    Ok(frame) => frame,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(handle, skipped, "L2CAP frame stream lagged");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => continue,
                };
                if frame.handle != handle { continue; }
                let packet = match Packet::parse(&frame.pdu) { Ok(packet) => packet, Err(error) => { tracing::warn!(handle, %error, "invalid L2CAP PDU"); continue; } };
                if packet.channel_id != SIGNALING_CID {
                    if let Some(channel) = channels.get(&packet.channel_id) {
                        // A slow consumer must not stall HCI credit returns or other controllers.
                        let _ = channel.sender.try_send(packet.payload);
                    }
                    continue;
                }
                discard_abandoned(&host, handle, &mut next_identifier, &mut pending, &mut accepts, &mut connection_ids, &mut configuration_ids).await;
                let signals = match Signal::parse_all(&packet.payload) { Ok(signals) => signals, Err(error) => { tracing::warn!(handle, %error, "invalid L2CAP signaling packet"); continue; } };
                for signal in signals {
                    tracing::debug!(handle, ?signal, "received L2CAP signal");
                    match signal {
                        Signal::ConnectionRequest { identifier, psm, source_cid } => {
                            if let Some(response) = accepts.get_mut(&psm).and_then(VecDeque::pop_front) {
                                accept_request(StoredRequest { identifier, psm, remote_cid: source_cid }, response, &host, handle, &mut next_cid, &mut next_identifier, &mut pending, &channels).await;
                            } else if let Some(outgoing_cid) = connection_ids
                                .values()
                                .copied()
                                .find(|cid| pending.get(cid).is_some_and(|channel| channel.psm == psm))
                            {
                                // Both peers may initiate the same HID PSM while
                                // authentication settles. Nintendo reports our
                                // request as pending and then sends its own. Merge
                                // the crossed requests by completing our caller with
                                // a freshly accepted channel. Reusing the outgoing CID
                                // leaves the peer's already-reserved endpoint ambiguous.
                                let local_cid = allocate_cid_for_psm(psm, &mut next_cid, &pending, &channels);
                                tracing::debug!(handle, psm, outgoing_cid, local_cid, source_cid, "coalescing crossed L2CAP connection requests");
                                connection_ids.retain(|_, cid| *cid != outgoing_cid);
                                let Some(outgoing) = pending.remove(&outgoing_cid) else {
                                    continue;
                                };
                                let superseded_remote_cid = outgoing.remote_cid;
                                pending.insert(
                                    local_cid,
                                    PendingChannel {
                                        psm,
                                        local_cid,
                                        remote_cid: source_cid,
                                        local_configured: false,
                                        remote_configured: false,
                                        response: outgoing.response,
                                    },
                                );
                                if superseded_remote_cid != 0 {
                                    let disconnect_id = allocate_identifier(&mut next_identifier);
                                    let disconnect = Signal::DisconnectionRequest {
                                        identifier: disconnect_id,
                                        destination_cid: superseded_remote_cid,
                                        source_cid: outgoing_cid,
                                    };
                                    let _ = send_signal(&host, handle, disconnect).await;
                                }
                                let response = Signal::ConnectionResponse {
                                    identifier,
                                    destination_cid: local_cid,
                                    source_cid,
                                    result: 0,
                                    status: 0,
                                };
                                if let Err(error) = send_signal(&host, handle, response).await {
                                    fail_pending(local_cid, error, &mut pending);
                                    continue;
                                }
                                let config_id = allocate_identifier(&mut next_identifier);
                                configuration_ids.insert(config_id, local_cid);
                                let config = Signal::ConfigurationRequest {
                                    identifier: config_id,
                                    destination_cid: source_cid,
                                    flags: 0,
                                    options: Vec::new(),
                                };
                                if let Err(error) = send_signal(&host, handle, config).await {
                                    fail_pending(local_cid, error, &mut pending);
                                }
                            } else {
                                stored_requests.push_back(StoredRequest { identifier, psm, remote_cid: source_cid });
                            }
                        }
                        Signal::ConnectionResponse { identifier, destination_cid, source_cid, result, .. } => {
                            let Some(local_cid) = connection_ids.get(&identifier).copied() else { continue; };
                            if result == 1 {
                                if destination_cid != 0
                                    && let Some(channel) = pending.get_mut(&local_cid)
                                {
                                    channel.remote_cid = destination_cid;
                                }
                                continue;
                            }
                            connection_ids.remove(&identifier);
                            if local_cid == 0 {
                                if result == 0 {
                                    let identifier = allocate_identifier(&mut next_identifier);
                                    let _ = send_signal(&host, handle, Signal::DisconnectionRequest { identifier, destination_cid, source_cid }).await;
                                }
                                continue;
                            }
                            if result != 0 || source_cid != local_cid {
                                fail_pending(local_cid, Error::L2cap(format!("connection to PSM 0x{:04x} rejected with result 0x{result:04x}", pending.get(&local_cid).map_or(0, |p| p.psm))), &mut pending);
                                continue;
                            }
                            if let Some(channel) = pending.get_mut(&local_cid) { channel.remote_cid = destination_cid; }
                            let config_id = allocate_identifier(&mut next_identifier);
                            configuration_ids.insert(config_id, local_cid);
                            let signal = Signal::ConfigurationRequest { identifier: config_id, destination_cid, flags: 0, options: Vec::new() };
                            if let Err(error) = send_signal(&host, handle, signal).await { fail_pending(local_cid, error, &mut pending); }
                        }
                        Signal::ConfigurationRequest { identifier, destination_cid, flags, options } => {
                            let (result, response_options) = ConfigurationOption::negotiate(options);
                            // Configuration Response carries the requester's Source CID,
                            // i.e. the peer's endpoint. Echoing our Destination CID makes
                            // the peer wait forever even though our local state looks ready.
                            let source_cid = pending
                                .get(&destination_cid)
                                .map(|channel| channel.remote_cid)
                                .or_else(|| channels.get(&destination_cid).map(|channel| channel.remote_cid))
                                .unwrap_or(destination_cid);
                            let response = Signal::ConfigurationResponse { identifier, source_cid, flags, result, options: response_options };
                            let _ = send_signal(&host, handle, response).await;
                            if result == 0 {
                                if let Some(channel) = pending.get_mut(&destination_cid) { channel.remote_configured = flags & 1 == 0; }
                                finish_if_ready(destination_cid, &command_sender, &mut pending, &mut channels);
                            }
                        }
                        Signal::ConfigurationResponse { identifier, source_cid, flags, result, .. } => {
                            let local_cid = configuration_ids
                                .remove(&identifier)
                                .unwrap_or(source_cid);
                            let Some(remote_cid) = pending.get(&local_cid).map(|channel| channel.remote_cid) else { continue; };
                            if result != 0 {
                                fail_pending(local_cid, Error::L2cap(format!("configuration rejected with result 0x{result:04x}")), &mut pending);
                                let identifier = allocate_identifier(&mut next_identifier);
                                let _ = send_signal(&host, handle, Signal::DisconnectionRequest { identifier, destination_cid: remote_cid, source_cid: local_cid }).await;
                                continue;
                            }
                            if flags & 1 != 0 {
                                let config_id = allocate_identifier(&mut next_identifier);
                                configuration_ids.insert(config_id, local_cid);
                                let signal = Signal::ConfigurationRequest { identifier: config_id, destination_cid: remote_cid, flags: 0, options: Vec::new() };
                                if let Err(error) = send_signal(&host, handle, signal).await { fail_pending(local_cid, error, &mut pending); }
                                continue;
                            }
                            if let Some(channel) = pending.get_mut(&local_cid) { channel.local_configured = true; }
                            finish_if_ready(local_cid, &command_sender, &mut pending, &mut channels);
                        }
                        Signal::DisconnectionRequest { identifier, destination_cid, source_cid } => {
                            channels.remove(&destination_cid);
                            fail_pending(destination_cid, Error::L2cap("peer disconnected during channel setup".into()), &mut pending);
                            let _ = send_signal(&host, handle, Signal::DisconnectionResponse { identifier, destination_cid, source_cid }).await;
                        }
                        Signal::EchoRequest { identifier, data } => { let _ = send_signal(&host, handle, Signal::EchoResponse { identifier, data }).await; }
                        Signal::InformationRequest { identifier, info_type } => {
                            let (result, data) = information_response(info_type);
                            let _ = send_signal(&host, handle, Signal::InformationResponse { identifier, info_type, result, data }).await;
                        }
                        Signal::CommandReject { identifier, reason, .. } => {
                            if let Some(local_cid) = connection_ids.remove(&identifier).or_else(|| configuration_ids.remove(&identifier)) {
                                fail_pending(local_cid, Error::L2cap(format!("signaling command rejected with reason 0x{reason:04x}")), &mut pending);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    for (_, channel) in pending {
        let _ = channel.response.send(Err(Error::ChannelClosed));
    }
    for (_, waiters) in accepts {
        for waiter in waiters {
            let _ = waiter.send(Err(Error::ChannelClosed));
        }
    }
}

fn information_response(info_type: u16) -> (u16, Bytes) {
    match info_type {
        // Connectionless MTU. Basic-mode HID itself uses dynamic channels,
        // but peers are allowed to query this mandatory information type.
        0x0001 => (0, Bytes::copy_from_slice(&DEFAULT_MTU.to_le_bytes())),
        // No extended L2CAP modes are implemented.
        0x0002 => (0, Bytes::copy_from_slice(&0u32.to_le_bytes())),
        // Signaling CID 0x0001 is the only fixed BR/EDR channel we expose.
        0x0003 => (0, Bytes::from_static(&[0x02, 0, 0, 0, 0, 0, 0, 0])),
        _ => (1, Bytes::new()), // not supported
    }
}

#[allow(clippy::too_many_arguments)]
async fn accept_request(
    request: StoredRequest,
    response: oneshot::Sender<Result<Channel>>,
    host: &HciHost,
    handle: u16,
    next_cid: &mut u16,
    next_identifier: &mut u8,
    pending: &mut HashMap<u16, PendingChannel>,
    channels: &HashMap<u16, OpenChannel>,
) {
    let local_cid = allocate_cid_for_psm(request.psm, next_cid, pending, channels);
    pending.insert(
        local_cid,
        PendingChannel {
            psm: request.psm,
            local_cid,
            remote_cid: request.remote_cid,
            local_configured: false,
            remote_configured: false,
            response,
        },
    );
    let connection_response = Signal::ConnectionResponse {
        identifier: request.identifier,
        destination_cid: local_cid,
        source_cid: request.remote_cid,
        result: 0,
        status: 0,
    };
    if let Err(error) = send_signal(host, handle, connection_response).await {
        fail_pending(local_cid, error, pending);
        return;
    }
    let config_id = allocate_identifier(next_identifier);
    let config = Signal::ConfigurationRequest {
        identifier: config_id,
        destination_cid: request.remote_cid,
        flags: 0,
        // The default BR/EDR MTU is sufficient for controller HID reports.
        // Nintendo's reconnect state machine expects the host side of the
        // control channel to use the option-free request seen from BlueZ.
        options: Vec::new(),
    };
    if let Err(error) = send_signal(host, handle, config).await {
        fail_pending(local_cid, error, pending);
    }
}

async fn discard_abandoned(
    host: &HciHost,
    handle: u16,
    next_identifier: &mut u8,
    pending: &mut HashMap<u16, PendingChannel>,
    accepts: &mut HashMap<u16, VecDeque<oneshot::Sender<Result<Channel>>>>,
    connection_ids: &mut HashMap<u8, u16>,
    configuration_ids: &mut HashMap<u8, u16>,
) {
    for waiters in accepts.values_mut() {
        waiters.retain(|waiter| !waiter.is_closed());
    }
    let abandoned: Vec<u16> = pending
        .iter()
        .filter(|(_, channel)| channel.response.is_closed())
        .map(|(local_cid, _)| *local_cid)
        .collect();
    for local_cid in abandoned {
        let Some(channel) = pending.remove(&local_cid) else {
            continue;
        };
        for cid in connection_ids
            .values_mut()
            .chain(configuration_ids.values_mut())
            .filter(|cid| **cid == local_cid)
        {
            *cid = 0;
        }
        if channel.remote_cid != 0 {
            let identifier = allocate_identifier(next_identifier);
            let disconnect = Signal::DisconnectionRequest {
                identifier,
                destination_cid: channel.remote_cid,
                source_cid: local_cid,
            };
            let _ = send_signal(host, handle, disconnect).await;
        }
    }
}

fn finish_if_ready(
    local_cid: u16,
    command_sender: &mpsc::WeakSender<LinkCommand>,
    pending: &mut HashMap<u16, PendingChannel>,
    channels: &mut HashMap<u16, OpenChannel>,
) {
    if !pending
        .get(&local_cid)
        .is_some_and(|channel| channel.local_configured && channel.remote_configured)
    {
        return;
    }
    let pending = pending.remove(&local_cid).unwrap();
    let (sender, receiver) = mpsc::channel(128);
    channels.insert(
        local_cid,
        OpenChannel {
            remote_cid: pending.remote_cid,
            sender,
        },
    );
    let Some(commands) = command_sender.upgrade() else {
        let _ = pending.response.send(Err(Error::ChannelClosed));
        return;
    };
    let channel = Channel {
        local_cid: pending.local_cid,
        remote_cid: pending.remote_cid,
        psm: pending.psm,
        commands,
        receiver,
    };
    let _ = pending.response.send(Ok(channel));
}

fn fail_pending(local_cid: u16, error: Error, pending: &mut HashMap<u16, PendingChannel>) {
    if let Some(channel) = pending.remove(&local_cid) {
        let _ = channel.response.send(Err(error));
    }
}

fn allocate_cid(
    next: &mut u16,
    pending: &HashMap<u16, PendingChannel>,
    channels: &HashMap<u16, OpenChannel>,
) -> u16 {
    loop {
        let candidate = *next;
        *next = if *next == u16::MAX {
            FIRST_DYNAMIC_CID
        } else {
            *next + 1
        };
        if !pending.contains_key(&candidate) && !channels.contains_key(&candidate) {
            return candidate;
        }
    }
}

fn allocate_cid_for_psm(
    psm: u16,
    next: &mut u16,
    pending: &HashMap<u16, PendingChannel>,
    channels: &HashMap<u16, OpenChannel>,
) -> u16 {
    let preferred = match psm {
        // Match the stable host CIDs used by BlueRetro's known-good Nintendo
        // page/reconnect path. They are scoped to one ACL link, so the same
        // values can be used for every physical controller.
        HID_CONTROL_PSM => Some(0x0080),
        HID_INTERRUPT_PSM => Some(0x0090),
        _ => None,
    };
    if let Some(cid) = preferred
        && !pending.contains_key(&cid)
        && !channels.contains_key(&cid)
    {
        return cid;
    }
    allocate_cid(next, pending, channels)
}

fn allocate_identifier(next: &mut u8) -> u8 {
    let identifier = (*next).max(1);
    *next = if identifier == u8::MAX {
        1
    } else {
        identifier + 1
    };
    identifier
}

async fn send_signal(host: &HciHost, handle: u16, signal: Signal) -> Result<()> {
    tracing::debug!(handle, ?signal, "sending L2CAP signal");
    host.send_l2cap(
        handle,
        &Packet::new(SIGNALING_CID, signal.encode()).encode(),
        false,
    )
    .await
}
