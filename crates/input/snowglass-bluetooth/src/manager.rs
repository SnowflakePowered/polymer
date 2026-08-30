//! High-level Bluetooth discovery and controller lifecycle manager.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
    time,
};

const ADAPTER_INITIALIZATION_ATTEMPTS: usize = 3;
const ADAPTER_INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(8);
const ADAPTER_INITIALIZATION_RETRY_DELAY: Duration = Duration::from_millis(500);
const COMMAND_QUEUE_ATTEMPTS: usize = 3;
const COMMAND_QUEUE_TIMEOUT: Duration = Duration::from_millis(250);
const COMMAND_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

use crate::{
    DeviceAddr, Error, Result, Transport, TransportKind,
    devices::{
        ConnectOptions, ControllerRegistry, ControllerSession, DeviceIdentity, PairingPolicy,
        ProfileId,
    },
    firmware::FirmwareRegistry,
    store::{ControllerLinkKeyStore, ControllerRecord, ControllerStore, MemoryControllerStore},
    transport::{
        ble::{BleDevice, BleGatt, default_adapter_address, scan as scan_ble, stop_scan},
        bredr::{
            BrEdrHid, LinkKeyStore, PairingAgent, PinCode,
            hci::{AdapterSelector, Event as HciEvent, HciHost, InquiryResult},
            l2cap::Link,
            sdp,
        },
    },
};

#[derive(Clone)]
pub struct ManagerConfig {
    pub bredr_adapter: AdapterSelector,
    pub ble_adapter_index: usize,
    pub controllers: ControllerRegistry,
    pub firmware: FirmwareRegistry,
    pub store: Arc<dyn ControllerStore>,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            bredr_adapter: AdapterSelector::first_winusb(),
            ble_adapter_index: 0,
            controllers: ControllerRegistry::standard(),
            firmware: FirmwareRegistry::embedded(),
            store: Arc::new(MemoryControllerStore::default()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanRequest {
    Ble { duration: Duration },
    BrEdr { duration_units: u8 },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ControllerId {
    pub transport: TransportKind,
    pub address: DeviceAddr,
}

impl ControllerId {
    pub const fn new(transport: TransportKind, address: DeviceAddr) -> Self {
        Self { transport, address }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SessionId(u64);

impl SessionId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug)]
pub struct ConnectedController {
    pub controller_id: ControllerId,
    pub session_id: SessionId,
    pub identity: DeviceIdentity,
    pub profile_id: ProfileId,
    pub player_index: u8,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum ManagerEvent {
    AdapterReady {
        transport: TransportKind,
        name: String,
    },
    AdapterUnavailable {
        transport: TransportKind,
        error: String,
    },
    ScanState {
        transport: TransportKind,
        active: bool,
    },
    Discovered {
        identity: DeviceIdentity,
        profile_id: Option<ProfileId>,
        rssi: Option<i16>,
    },
    Connecting {
        controller_id: ControllerId,
        profile_id: ProfileId,
    },
    Connected(ConnectedController),
    RawReport {
        controller_id: ControllerId,
        session_id: SessionId,
        received_at: std::time::Instant,
        bytes: Bytes,
    },
    Disconnected {
        controller_id: ControllerId,
        session_id: SessionId,
        reason: String,
        reconnecting: bool,
    },
    FirmwareLoaded {
        family: String,
    },
    Error {
        address: Option<DeviceAddr>,
        message: String,
    },
}

enum Command {
    Scan(ScanRequest, oneshot::Sender<Result<()>>),
    ScanFinished {
        transport: TransportKind,
        discovered: HashMap<DeviceAddr, Discovered>,
        result: Result<()>,
        reply: oneshot::Sender<Result<()>>,
    },
    StopScan(TransportKind, oneshot::Sender<Result<()>>),
    Connect(DeviceAddr, ConnectOptions, oneshot::Sender<Result<()>>),
    Send(SessionId, Vec<u8>, oneshot::Sender<Result<()>>),
    SetPlayerIndex(SessionId, u8, oneshot::Sender<Result<()>>),
    SetRumble(SessionId, u16, u16, oneshot::Sender<Result<()>>),
    Disconnect(SessionId, bool, oneshot::Sender<Result<()>>),
    Forget(ControllerId, oneshot::Sender<Result<()>>),
    WakeDiscovered(ControllerRecord, BleDevice),
    SessionEnded {
        controller_id: ControllerId,
        session_id: SessionId,
        reason: String,
    },
    Shutdown(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct BluetoothManager {
    commands: mpsc::Sender<Command>,
    events: broadcast::Sender<ManagerEvent>,
}

impl BluetoothManager {
    pub async fn start(config: ManagerConfig) -> Result<Self> {
        let (events, _) = broadcast::channel(1024);
        let (commands, receiver) = mpsc::channel(64);
        let actor_events = events.clone();
        let internal = commands.clone();
        tokio::spawn(async move {
            BluetoothServer::new(config, receiver, internal, actor_events)
                .run()
                .await;
        });
        Ok(Self { commands, events })
    }

    pub fn events(&self) -> broadcast::Receiver<ManagerEvent> {
        self.events.subscribe()
    }

    pub async fn scan(&self, request: ScanRequest) -> Result<()> {
        self.call(|reply| Command::Scan(request, reply)).await
    }
    pub async fn stop_scan(&self, transport: TransportKind) -> Result<()> {
        self.call(|reply| Command::StopScan(transport, reply)).await
    }
    pub async fn connect(&self, address: DeviceAddr, options: ConnectOptions) -> Result<()> {
        self.call(|reply| Command::Connect(address, options, reply))
            .await
    }
    pub async fn send_raw(&self, session_id: SessionId, bytes: &[u8]) -> Result<()> {
        self.call(|reply| Command::Send(session_id, bytes.to_vec(), reply))
            .await
    }
    pub async fn set_player_index(&self, session_id: SessionId, index: u8) -> Result<()> {
        self.call(|reply| Command::SetPlayerIndex(session_id, index, reply))
            .await
    }
    pub async fn set_rumble(
        &self,
        session_id: SessionId,
        low_frequency: u16,
        high_frequency: u16,
    ) -> Result<()> {
        self.call(|reply| Command::SetRumble(session_id, low_frequency, high_frequency, reply))
            .await
    }
    pub async fn sleep(&self, session_id: SessionId) -> Result<()> {
        self.call(|reply| Command::Disconnect(session_id, true, reply))
            .await
    }
    pub async fn disconnect(&self, session_id: SessionId) -> Result<()> {
        self.call(|reply| Command::Disconnect(session_id, false, reply))
            .await
    }
    pub async fn forget(&self, controller_id: ControllerId) -> Result<()> {
        self.call(|reply| Command::Forget(controller_id, reply))
            .await
    }
    pub async fn shutdown(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Shutdown(tx))
            .await
            .map_err(|_| Error::ChannelClosed)?;
        rx.await.map_err(|_| Error::ChannelClosed)
    }

    async fn call<F>(&self, build: F) -> Result<()>
    where
        F: FnOnce(oneshot::Sender<Result<()>>) -> Command,
    {
        let (tx, rx) = oneshot::channel();
        let mut permit = None;
        for attempt in 1..=COMMAND_QUEUE_ATTEMPTS {
            match time::timeout(COMMAND_QUEUE_TIMEOUT, self.commands.reserve()).await {
                Ok(Ok(available)) => {
                    permit = Some(available);
                    break;
                }
                Ok(Err(_)) => return Err(Error::ChannelClosed),
                Err(_) if attempt < COMMAND_QUEUE_ATTEMPTS => tokio::task::yield_now().await,
                Err(_) => return Err(Error::Timeout("Bluetooth command queue")),
            }
        }
        permit
            .expect("a command permit is present after a successful reservation")
            .send(build(tx));
        time::timeout(COMMAND_RESPONSE_TIMEOUT, rx)
            .await
            .map_err(|_| Error::Timeout("Bluetooth command response"))?
            .map_err(|_| Error::ChannelClosed)?
    }
}

struct Discovered {
    identity: DeviceIdentity,
    bredr: Option<InquiryResult>,
    ble: Option<BleDevice>,
}

struct Session {
    id: SessionId,
    controller_id: ControllerId,
    identity: DeviceIdentity,
    player_index: u8,
    transport: Box<dyn Transport>,
    controller: Box<dyn ControllerSession>,
    report_task: JoinHandle<()>,
}

struct ConnectContext<'a> {
    config: &'a ManagerConfig,
    host: Option<&'a Arc<HciHost>>,
    pairing: Option<&'a Arc<PairingAgent>>,
    keys: &'a ControllerLinkKeyStore,
    events: &'a broadcast::Sender<ManagerEvent>,
}

struct BluetoothServer {
    config: ManagerConfig,
    commands: mpsc::Receiver<Command>,
    internal: mpsc::Sender<Command>,
    events: broadcast::Sender<ManagerEvent>,
}

impl BluetoothServer {
    fn new(
        config: ManagerConfig,
        commands: mpsc::Receiver<Command>,
        internal: mpsc::Sender<Command>,
        events: broadcast::Sender<ManagerEvent>,
    ) -> Self {
        Self {
            config,
            commands,
            internal,
            events,
        }
    }

    async fn run(self) {
        let Self {
            config,
            mut commands,
            internal,
            events,
        } = self;
        let (bredr, ble_names) = tokio::join!(
            Self::initialize_adapter("BR/EDR adapter", || {
                HciHost::open_with_firmware(&config.bredr_adapter, config.firmware.clone())
            }),
            Self::initialize_adapter("BLE adapter", BleGatt::adapter_names),
        );
        let bredr = match bredr {
            Ok(host) => {
                let host = Arc::new(host);
                let info = host.info();
                if let Some(source) = &info.firmware_source {
                    let _ = events.send(ManagerEvent::FirmwareLoaded {
                        family: source.clone(),
                    });
                }
                let _ = events.send(ManagerEvent::AdapterReady {
                    transport: TransportKind::BrEdr,
                    name: format!(
                        "{:04x}:{:04x} ({})",
                        info.usb_vendor_id, info.usb_product_id, info.address
                    ),
                });
                Some(host)
            }
            Err(error) => {
                let _ = events.send(ManagerEvent::AdapterUnavailable {
                    transport: TransportKind::BrEdr,
                    error: error.to_string(),
                });
                None
            }
        };
        match ble_names {
            Ok(names) if config.ble_adapter_index < names.len() => {
                let _ = events.send(ManagerEvent::AdapterReady {
                    transport: TransportKind::Ble,
                    name: names[config.ble_adapter_index].clone(),
                });
            }
            Ok(_) => {
                let _ = events.send(ManagerEvent::AdapterUnavailable {
                    transport: TransportKind::Ble,
                    error: "selected OS BLE adapter does not exist".into(),
                });
            }
            Err(error) => {
                let _ = events.send(ManagerEvent::AdapterUnavailable {
                    transport: TransportKind::Ble,
                    error: error.to_string(),
                });
            }
        }

        let keys = Arc::new(ControllerLinkKeyStore::new(config.store.clone()));
        let pairing = bredr
            .as_ref()
            .map(|host| Arc::new(PairingAgent::new(host.clone(), keys.clone())));
        let pairing_task = pairing
            .as_ref()
            .map(|agent| tokio::spawn(agent.clone().run()));
        let mut discovered = HashMap::<DeviceAddr, Discovered>::new();
        let mut active_scans = HashSet::<TransportKind>::new();
        let mut scan_tasks = HashMap::<TransportKind, JoinHandle<()>>::new();
        let mut sessions = HashMap::<DeviceAddr, Session>::new();
        let mut next_session_id = 1_u64;
        let mut hci_events = bredr.as_ref().map(|host| host.events());
        let wake_gate = Arc::new(tokio::sync::Mutex::new(()));
        let mut wake_tasks = HashMap::<DeviceAddr, JoinHandle<()>>::new();
        for record in config.store.records().unwrap_or_default() {
            if config.controllers.get(&record.profile_id).is_none() {
                let _ = events.send(ManagerEvent::Error {
                    address: Some(record.address),
                    message: Error::MissingProfile(record.profile_id.to_string()).to_string(),
                });
            } else if record.transport == TransportKind::Ble && record.auto_reconnect {
                Self::arm_ble_wake(
                    record,
                    config.controllers.clone(),
                    internal.clone(),
                    wake_gate.clone(),
                    &mut wake_tasks,
                );
            }
        }

        loop {
            let command = tokio::select! {
                command = commands.recv() => command,
                event = Self::receive_hci(&mut hci_events) => {
                    match event {
                        Some(Ok(HciEvent::ConnectionRequest { addr, .. })) => {
                            if sessions.contains_key(&addr) { continue; }
                            let session_id = SessionId(next_session_id);
                            next_session_id = next_session_id.wrapping_add(1).max(1);
                            match Self::accept_inbound(
                                &config,
                                bredr.as_ref().unwrap(),
                                addr,
                                session_id,
                                &events,
                                internal.clone(),
                            ).await {
                                Ok(session) => { sessions.insert(addr, session); }
                                Err(error) => {
                                    let _ = bredr.as_ref().unwrap().reject_connection(addr).await;
                                    let _ = events.send(ManagerEvent::Error { address: Some(addr), message: error.to_string() });
                                }
                            }
                        }
                        Some(Err(broadcast::error::RecvError::Lagged(count))) => {
                            let _ = events.send(ManagerEvent::Error { address: None, message: format!("dropped {count} HCI lifecycle events") });
                        }
                        _ => {}
                    }
                    continue;
                }
            };
            let Some(command) = command else {
                break;
            };
            match command {
                Command::Scan(request, reply) => {
                    let transport = match request {
                        ScanRequest::Ble { .. } => TransportKind::Ble,
                        ScanRequest::BrEdr { .. } => TransportKind::BrEdr,
                    };
                    if !active_scans.insert(transport) {
                        let _ = reply.send(Err(Error::Unsupported(
                            "a scan is already active on this transport",
                        )));
                        continue;
                    }
                    let scan_config = config.clone();
                    let scan_host = bredr.clone();
                    let scan_events = events.clone();
                    let scan_gate = wake_gate.clone();
                    let commands = internal.clone();
                    let task = tokio::spawn(async move {
                        let mut found = HashMap::new();
                        let result = Self::scan(
                            &scan_config,
                            scan_host.as_ref(),
                            request,
                            &scan_events,
                            &mut found,
                            &scan_gate,
                        )
                        .await;
                        let _ = commands
                            .send(Command::ScanFinished {
                                transport,
                                discovered: found,
                                result,
                                reply,
                            })
                            .await;
                    });
                    scan_tasks.insert(transport, task);
                }
                Command::ScanFinished {
                    transport,
                    discovered: found,
                    result,
                    reply,
                } => {
                    active_scans.remove(&transport);
                    scan_tasks.remove(&transport);
                    discovered.extend(found);
                    let _ = reply.send(result);
                }
                Command::StopScan(kind, reply) => {
                    let result = match kind {
                        TransportKind::Ble => stop_scan(config.ble_adapter_index).await,
                        TransportKind::BrEdr => match &bredr {
                            Some(host) => host.cancel_inquiry().await,
                            None => Err(Error::AdapterNotFound),
                        },
                    };
                    let _ = reply.send(result);
                }
                Command::Connect(address, options, reply) => {
                    let session_id = SessionId(next_session_id);
                    next_session_id = next_session_id.wrapping_add(1).max(1);
                    let result = Self::connect(
                        ConnectContext {
                            config: &config,
                            host: bredr.as_ref(),
                            pairing: pairing.as_ref(),
                            keys: keys.as_ref(),
                            events: &events,
                        },
                        address,
                        session_id,
                        options,
                        &discovered,
                        &mut sessions,
                        internal.clone(),
                    )
                    .await;
                    let _ = reply.send(result);
                }
                Command::Send(session_id, bytes, reply) => {
                    let result = match Self::session_mut(&mut sessions, session_id) {
                        Some(session) => {
                            session
                                .controller
                                .send_raw_hid(session.transport.as_mut(), &bytes)
                                .await
                        }
                        None => Err(Error::Unsupported("controller is not connected")),
                    };
                    let _ = reply.send(result);
                }
                Command::SetPlayerIndex(session_id, player_index, reply) => {
                    let result = match Self::session_mut(&mut sessions, session_id) {
                        Some(session) => {
                            let result = session
                                .controller
                                .set_player_led(session.transport.as_mut(), player_index)
                                .await;
                            if result.is_ok() {
                                session.player_index = player_index;
                                if let Err(error) = Self::persist_session(&config, session) {
                                    let _ = events.send(ManagerEvent::Error {
                                        address: Some(session.controller_id.address),
                                        message: format!("failed to persist player index: {error}"),
                                    });
                                }
                            }
                            result
                        }
                        None => Err(Error::Unsupported("controller session is not connected")),
                    };
                    let _ = reply.send(result);
                }
                Command::SetRumble(session_id, low_frequency, high_frequency, reply) => {
                    let result = match Self::session_mut(&mut sessions, session_id) {
                        Some(session) => {
                            session
                                .controller
                                .set_rumble(
                                    session.transport.as_mut(),
                                    low_frequency,
                                    high_frequency,
                                )
                                .await
                        }
                        None => Err(Error::Unsupported("controller session is not connected")),
                    };
                    let _ = reply.send(result);
                }
                Command::Disconnect(session_id, sleep, reply) => {
                    let controller_id =
                        Self::session(&sessions, session_id).map(|session| session.controller_id);
                    let result = Self::disconnect(session_id, sleep, &events, &mut sessions).await;
                    if sleep
                        && result.is_ok()
                        && let Some(controller_id) = controller_id
                        && let Some(record) = config
                            .store
                            .records()
                            .unwrap_or_default()
                            .into_iter()
                            .find(|record| {
                                record.address == controller_id.address
                                    && record.transport == TransportKind::Ble
                                    && record.auto_reconnect
                            })
                    {
                        Self::arm_ble_wake(
                            record,
                            config.controllers.clone(),
                            internal.clone(),
                            wake_gate.clone(),
                            &mut wake_tasks,
                        );
                    }
                    let _ = reply.send(result);
                }
                Command::Forget(controller_id, reply) => {
                    if let Some(task) = wake_tasks.remove(&controller_id.address) {
                        task.abort();
                    }
                    let active_session_id = sessions
                        .values()
                        .find(|session| session.controller_id == controller_id)
                        .map(|session| session.id);
                    if let Some(session_id) = active_session_id {
                        let _ = Self::disconnect(session_id, false, &events, &mut sessions).await;
                    }
                    if let Some(pairing) = &pairing {
                        let _ = pairing.forget(controller_id.address);
                    }
                    let _ = reply.send(config.store.remove(controller_id.address));
                }
                Command::WakeDiscovered(record, device) => {
                    wake_tasks.remove(&record.address);
                    let identity = Self::ble_identity(&device);
                    let publish = Self::publish_discovery(
                        &config,
                        identity,
                        None,
                        Some(device),
                        &events,
                        &mut discovered,
                    );
                    let options = ConnectOptions {
                        bonding_mode: None,
                        player_index: record.player_index,
                        auto_reconnect: true,
                    };
                    let result = match publish {
                        Ok(()) => {
                            Self::connect(
                                ConnectContext {
                                    config: &config,
                                    host: bredr.as_ref(),
                                    pairing: pairing.as_ref(),
                                    keys: keys.as_ref(),
                                    events: &events,
                                },
                                record.address,
                                SessionId(next_session_id),
                                options,
                                &discovered,
                                &mut sessions,
                                internal.clone(),
                            )
                            .await
                        }
                        Err(error) => Err(error),
                    };
                    next_session_id = next_session_id.wrapping_add(1).max(1);
                    if let Err(error) = result {
                        let _ = events.send(ManagerEvent::Error {
                            address: Some(record.address),
                            message: format!("automatic reconnect failed: {error}"),
                        });
                        Self::arm_ble_wake(
                            record,
                            config.controllers.clone(),
                            internal.clone(),
                            wake_gate.clone(),
                            &mut wake_tasks,
                        );
                    }
                }
                Command::SessionEnded {
                    controller_id,
                    session_id,
                    reason,
                } => {
                    let is_current = sessions
                        .get(&controller_id.address)
                        .is_some_and(|session| session.id == session_id);
                    if !is_current {
                        continue;
                    }
                    let session = sessions
                        .remove(&controller_id.address)
                        .expect("session checked above");
                    let wake_record =
                        (session.identity.transport == TransportKind::Ble)
                            .then(|| {
                                config.store.records().unwrap_or_default().into_iter().find(
                                    |record| {
                                        record.address == controller_id.address
                                            && record.auto_reconnect
                                    },
                                )
                            })
                            .flatten();
                    let _ = events.send(ManagerEvent::Disconnected {
                        controller_id,
                        session_id,
                        reason,
                        reconnecting: wake_record.is_some(),
                    });
                    if let Some(record) = wake_record {
                        Self::arm_ble_wake(
                            record,
                            config.controllers.clone(),
                            internal.clone(),
                            wake_gate.clone(),
                            &mut wake_tasks,
                        );
                    }
                }
                Command::Shutdown(reply) => {
                    for (_, task) in scan_tasks.drain() {
                        task.abort();
                    }
                    for (_, task) in wake_tasks.drain() {
                        task.abort();
                    }
                    for (_, mut session) in sessions.drain() {
                        session.report_task.abort();
                        let _ = session.transport.disconnect().await;
                    }
                    let _ = reply.send(());
                    break;
                }
            }
        }
        if let Some(task) = pairing_task {
            task.abort();
        }
    }

    async fn initialize_adapter<T, F, Future>(label: &'static str, mut initialize: F) -> Result<T>
    where
        F: FnMut() -> Future,
        Future: std::future::Future<Output = Result<T>>,
    {
        for attempt in 1..=ADAPTER_INITIALIZATION_ATTEMPTS {
            let result = time::timeout(ADAPTER_INITIALIZATION_TIMEOUT, initialize()).await;
            match result {
                Ok(Ok(adapter)) => return Ok(adapter),
                Ok(Err(error)) if attempt == ADAPTER_INITIALIZATION_ATTEMPTS => return Err(error),
                Err(_) if attempt == ADAPTER_INITIALIZATION_ATTEMPTS => {
                    return Err(Error::Timeout(label));
                }
                Ok(Err(error)) => {
                    tracing::warn!(attempt, %error, label, "retrying Bluetooth adapter initialization");
                }
                Err(_) => {
                    tracing::warn!(
                        attempt,
                        label,
                        "Bluetooth adapter initialization timed out; retrying"
                    );
                }
            }
            time::sleep(ADAPTER_INITIALIZATION_RETRY_DELAY).await;
        }
        unreachable!("adapter initialization attempts are nonzero")
    }

    fn arm_ble_wake(
        record: ControllerRecord,
        controllers: ControllerRegistry,
        commands: mpsc::Sender<Command>,
        gate: Arc<tokio::sync::Mutex<()>>,
        tasks: &mut HashMap<DeviceAddr, JoinHandle<()>>,
    ) {
        if tasks.contains_key(&record.address) {
            return;
        }
        let address = record.address;
        let adapter = record.adapter_index.unwrap_or(0);
        let task = tokio::spawn(async move {
            let mut backoff = Duration::from_secs(2);
            let local_address = default_adapter_address().await.ok();
            loop {
                let found = {
                    let _guard = gate.lock().await;
                    scan_ble(adapter, Duration::from_secs(4))
                        .await
                        .ok()
                        .and_then(|devices| {
                            let controller = controllers.get(&record.profile_id)?;
                            devices.into_iter().find(|device| {
                                device.addr == address
                                    && controller.accepts_reconnect_advertisement(
                                        &Self::ble_identity(device),
                                        local_address,
                                    )
                            })
                        })
                };
                if let Some(device) = found {
                    let _ = commands.send(Command::WakeDiscovered(record, device)).await;
                    break;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        });
        tasks.insert(address, task);
    }

    async fn scan(
        config: &ManagerConfig,
        bredr: Option<&Arc<HciHost>>,
        request: ScanRequest,
        events: &broadcast::Sender<ManagerEvent>,
        discovered: &mut HashMap<DeviceAddr, Discovered>,
        wake_gate: &Arc<tokio::sync::Mutex<()>>,
    ) -> Result<()> {
        let kind = match request {
            ScanRequest::Ble { .. } => TransportKind::Ble,
            ScanRequest::BrEdr { .. } => TransportKind::BrEdr,
        };
        let _ = events.send(ManagerEvent::ScanState {
            transport: kind,
            active: true,
        });
        let result = match request {
            ScanRequest::Ble { duration } => {
                let devices = {
                    let _guard = wake_gate.lock().await;
                    scan_ble(config.ble_adapter_index, duration).await?
                };
                for device in devices {
                    let identity = Self::ble_identity(&device);
                    Self::publish_discovery(
                        config,
                        identity,
                        None,
                        Some(device),
                        events,
                        discovered,
                    )?;
                }
                Ok(())
            }
            ScanRequest::BrEdr { duration_units } => {
                let host = bredr.ok_or(Error::AdapterNotFound)?;
                for inquiry in host.inquiry(duration_units, 0).await? {
                    let name = host.remote_name(&inquiry).await.ok();
                    let identity = DeviceIdentity {
                        address: inquiry.addr,
                        transport: TransportKind::BrEdr,
                        name,
                        class_of_device: Some(u32::from_le_bytes([
                            inquiry.class_of_device[0],
                            inquiry.class_of_device[1],
                            inquiry.class_of_device[2],
                            0,
                        ])),
                        service_uuids: Vec::new(),
                        manufacturer_data: Vec::new(),
                        pnp: None,
                    };
                    Self::publish_discovery(
                        config,
                        identity,
                        Some(inquiry),
                        None,
                        events,
                        discovered,
                    )?;
                }
                Ok(())
            }
        };
        let _ = events.send(ManagerEvent::ScanState {
            transport: kind,
            active: false,
        });
        result
    }

    fn ble_identity(device: &BleDevice) -> DeviceIdentity {
        DeviceIdentity {
            address: device.addr,
            transport: TransportKind::Ble,
            name: device.name.clone(),
            class_of_device: None,
            service_uuids: device.services.clone(),
            manufacturer_data: device
                .manufacturer_data
                .iter()
                .map(|(id, bytes)| (*id, bytes.clone()))
                .collect(),
            pnp: None,
        }
    }

    fn publish_discovery(
        config: &ManagerConfig,
        identity: DeviceIdentity,
        bredr: Option<InquiryResult>,
        ble: Option<BleDevice>,
        events: &broadcast::Sender<ManagerEvent>,
        discovered: &mut HashMap<DeviceAddr, Discovered>,
    ) -> Result<()> {
        let profile_id = config
            .controllers
            .identify(&identity)?
            .map(|controller| controller.id());
        let rssi = ble
            .as_ref()
            .and_then(|device| device.rssi)
            .or_else(|| bredr.as_ref().and_then(|device| device.rssi).map(i16::from));
        let _ = events.send(ManagerEvent::Discovered {
            identity: identity.clone(),
            profile_id,
            rssi,
        });
        discovered.insert(
            identity.address,
            Discovered {
                identity,
                bredr,
                ble,
            },
        );
        Ok(())
    }

    async fn connect(
        context: ConnectContext<'_>,
        address: DeviceAddr,
        session_id: SessionId,
        options: ConnectOptions,
        discovered: &HashMap<DeviceAddr, Discovered>,
        sessions: &mut HashMap<DeviceAddr, Session>,
        internal: mpsc::Sender<Command>,
    ) -> Result<()> {
        let ConnectContext {
            config,
            host,
            pairing,
            keys,
            events,
        } = context;
        if sessions.contains_key(&address) {
            return Ok(());
        }
        let device = discovered.get(&address).ok_or(Error::Unsupported(
            "device must be discovered before connecting",
        ))?;
        let mut identity = device.identity.clone();
        let mut identified_handle = None;
        if identity.transport == TransportKind::BrEdr {
            let preliminary = config.controllers.identify(&identity)?;
            if preliminary
                .as_ref()
                .is_none_or(|controller| controller.id().as_str() == "generic.hid")
            {
                let host = host.ok_or(Error::AdapterNotFound)?;
                let inquiry = device
                    .bredr
                    .as_ref()
                    .ok_or(Error::InvalidPacket("missing BR/EDR inquiry data"))?;
                let handle = host.connect(inquiry).await?;
                match sdp::query_identity(&Link::new(host.clone(), handle)).await {
                    Ok(sdp) => {
                        identity.service_uuids = sdp.services;
                        identity.pnp = sdp.pnp;
                    }
                    Err(error) => {
                        tracing::debug!(%address, %error, "SDP identity query failed; retaining discovery identity")
                    }
                }
                identified_handle = Some(handle);
            }
        }
        let controller = config
            .controllers
            .identify(&identity)?
            .ok_or(Error::Unsupported(
                "no controller profile matched the device",
            ))?;
        let profile_id = controller.id();
        let controller_id = ControllerId::new(identity.transport, address);
        let _ = events.send(ManagerEvent::Connecting {
            controller_id,
            profile_id: profile_id.clone(),
        });
        let policy = controller.connection_policy(&identity, &options);
        let mut controller_session = controller.connect(&options);
        let mut transport: Box<dyn Transport> = match device.identity.transport {
            TransportKind::BrEdr => {
                let host = host.ok_or(Error::AdapterNotFound)?;
                let inquiry = device
                    .bredr
                    .as_ref()
                    .ok_or(Error::InvalidPacket("missing BR/EDR inquiry data"))?;
                host.set_host_identity(
                    policy.host_identity.name,
                    policy.host_identity.class_of_device,
                )
                .await?;
                if let Some(pairing) = pairing {
                    let pin = match policy.pairing {
                        PairingPolicy::FixedPin(pin) => Some(PinCode::Fixed(pin.to_vec())),
                        PairingPolicy::RemoteAddressPin => Some(PinCode::RemoteAddress),
                        PairingPolicy::LocalAddressPin => Some(PinCode::LocalAddress),
                        _ => None,
                    };
                    if let Some(pin) = pin {
                        pairing.set_pin(address, pin)?;
                    }
                }
                let mut hid = BrEdrHid::new(host.clone());
                if let Some(slots) = policy.automatic_flush_slots {
                    hid.set_automatic_flush_slots(slots);
                }
                let connect_result = if let Some(handle) = identified_handle {
                    hid.establish_handle(handle, policy.controller_opens_hid)
                        .await
                } else if policy.controller_opens_hid {
                    hid.connect_discovered_device_initiated_hid(inquiry).await
                } else {
                    hid.connect_discovered(inquiry).await
                };
                let restore_result = host
                    .set_host_identity("Snowglass", [0x0c, 0x01, 0x1c])
                    .await;
                connect_result?;
                restore_result?;
                Box::new(hid)
            }
            TransportKind::Ble => {
                let _ble = device
                    .ble
                    .as_ref()
                    .ok_or(Error::InvalidPacket("missing BLE discovery data"))?;
                let gatt_profile =
                    controller
                        .gatt_profile(&identity)?
                        .ok_or(Error::Unsupported(
                            "controller profile does not provide a BLE GATT configuration",
                        ))?;
                let mut gatt = BleGatt::new(config.ble_adapter_index, gatt_profile);
                gatt.connect(address).await?;
                if matches!(policy.pairing, PairingPolicy::PseudoOob) {
                    let host_address = default_adapter_address().await?;
                    controller.bond(&mut gatt, &identity, host_address).await?;
                }
                Box::new(gatt)
            }
        };
        controller_session.synchronize(transport.as_mut()).await?;
        let stored_key = keys.get(address)?;
        config.store.put(ControllerRecord {
            address,
            transport: identity.transport,
            profile_id: profile_id.clone(),
            link_key: stored_key.map(|key| key.value),
            link_key_type: stored_key.map(|key| key.key_type),
            adapter_index: (identity.transport == TransportKind::Ble)
                .then_some(config.ble_adapter_index),
            auto_reconnect: options.auto_reconnect,
            player_index: options.player_index,
        })?;
        let report_task = Self::spawn_reports(
            controller_id,
            session_id,
            transport.reports(),
            transport.disconnections(),
            events.clone(),
            internal,
        );
        sessions.insert(
            address,
            Session {
                id: session_id,
                controller_id,
                identity: identity.clone(),
                player_index: options.player_index,
                transport,
                controller: controller_session,
                report_task,
            },
        );
        let _ = events.send(ManagerEvent::Connected(ConnectedController {
            controller_id,
            session_id,
            identity,
            profile_id,
            player_index: options.player_index,
        }));
        Ok(())
    }

    async fn accept_inbound(
        config: &ManagerConfig,
        host: &Arc<HciHost>,
        address: DeviceAddr,
        session_id: SessionId,
        events: &broadcast::Sender<ManagerEvent>,
        internal: mpsc::Sender<Command>,
    ) -> Result<Session> {
        let record = config
            .store
            .records()?
            .into_iter()
            .find(|record| {
                record.address == address
                    && record.transport == TransportKind::BrEdr
                    && record.auto_reconnect
            })
            .ok_or(Error::Unsupported(
                "inbound connection is not from a persisted controller",
            ))?;
        let controller = config
            .controllers
            .get(&record.profile_id)
            .ok_or_else(|| Error::MissingProfile(record.profile_id.to_string()))?;
        let identity = DeviceIdentity {
            address,
            transport: TransportKind::BrEdr,
            name: None,
            class_of_device: None,
            service_uuids: Vec::new(),
            manufacturer_data: Vec::new(),
            pnp: None,
        };
        let options = ConnectOptions {
            bonding_mode: None,
            player_index: record.player_index,
            auto_reconnect: true,
        };
        let policy = controller.connection_policy(&identity, &options);
        let frames = host.l2cap_frames();
        let connection_events = host.events();
        host.set_host_identity(
            policy.host_identity.name,
            policy.host_identity.class_of_device,
        )
        .await?;
        let handle_result = host
            .accept_connection_with_role(address, policy.become_master)
            .await;
        let restore_result = host
            .set_host_identity("Snowglass", [0x0c, 0x01, 0x1c])
            .await;
        let handle = handle_result?;
        restore_result?;
        let mut hid = BrEdrHid::new(host.clone());
        if let Some(slots) = policy.automatic_flush_slots {
            hid.set_automatic_flush_slots(slots);
        }
        let attach_result = if policy.wait_for_encryption_before_inbound_hid {
            hid.attach_inbound_encrypted_with_frames(handle, frames, connection_events)
                .await
        } else {
            hid.attach_inbound_with_frames(handle, frames).await
        };
        if let Err(error) = attach_result {
            let _ = host.disconnect(handle).await;
            return Err(error);
        }
        let mut transport: Box<dyn Transport> = Box::new(hid);
        let mut controller_session = controller.connect(&options);
        if let Err(error) = controller_session.synchronize(transport.as_mut()).await {
            let _ = transport.disconnect().await;
            return Err(error);
        }
        let controller_id = ControllerId::new(TransportKind::BrEdr, address);
        let report_task = Self::spawn_reports(
            controller_id,
            session_id,
            transport.reports(),
            transport.disconnections(),
            events.clone(),
            internal,
        );
        let _ = events.send(ManagerEvent::Connected(ConnectedController {
            controller_id,
            session_id,
            identity: identity.clone(),
            profile_id: record.profile_id.clone(),
            player_index: record.player_index,
        }));
        Ok(Session {
            id: session_id,
            controller_id,
            identity,
            player_index: record.player_index,
            transport,
            controller: controller_session,
            report_task,
        })
    }

    fn spawn_reports(
        controller_id: ControllerId,
        session_id: SessionId,
        mut reports: crate::ReportReceiver,
        mut disconnections: Option<crate::DisconnectReceiver>,
        events: broadcast::Sender<ManagerEvent>,
        internal: mpsc::Sender<Command>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let reason = loop {
                tokio::select! {
                    report = reports.recv() => {
                        match report {
                            Ok(bytes) => {
                                let _ = events.send(ManagerEvent::RawReport {
                                    controller_id,
                                    session_id,
                                    received_at: std::time::Instant::now(),
                                    bytes,
                                });
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(broadcast::error::RecvError::Closed) => {
                                break "input report stream closed".to_owned();
                            }
                        }
                    }
                    disconnected = Self::receive_disconnect(&mut disconnections) => {
                        if disconnected {
                            break "physical controller disconnected".to_owned();
                        }
                    }
                }
            };
            let _ = internal
                .send(Command::SessionEnded {
                    controller_id,
                    session_id,
                    reason,
                })
                .await;
        })
    }

    async fn receive_disconnect(receiver: &mut Option<crate::DisconnectReceiver>) -> bool {
        match receiver {
            Some(receiver) => loop {
                match receiver.recv().await {
                    Ok(()) | Err(broadcast::error::RecvError::Closed) => return true,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            },
            None => std::future::pending().await,
        }
    }

    async fn receive_hci(
        receiver: &mut Option<broadcast::Receiver<HciEvent>>,
    ) -> Option<std::result::Result<HciEvent, broadcast::error::RecvError>> {
        match receiver {
            Some(receiver) => Some(receiver.recv().await),
            None => std::future::pending().await,
        }
    }

    fn session(sessions: &HashMap<DeviceAddr, Session>, session_id: SessionId) -> Option<&Session> {
        sessions.values().find(|session| session.id == session_id)
    }

    fn session_mut(
        sessions: &mut HashMap<DeviceAddr, Session>,
        session_id: SessionId,
    ) -> Option<&mut Session> {
        sessions
            .values_mut()
            .find(|session| session.id == session_id)
    }

    fn persist_session(config: &ManagerConfig, session: &Session) -> Result<()> {
        let Some(mut record) = config
            .store
            .records()?
            .into_iter()
            .find(|record| record.address == session.controller_id.address)
        else {
            return Err(Error::Unsupported(
                "connected controller has no persisted record",
            ));
        };
        record.player_index = session.player_index;
        config.store.put(record)
    }

    async fn disconnect(
        session_id: SessionId,
        sleep: bool,
        events: &broadcast::Sender<ManagerEvent>,
        sessions: &mut HashMap<DeviceAddr, Session>,
    ) -> Result<()> {
        let Some(address) = sessions
            .iter()
            .find(|(_, session)| session.id == session_id)
            .map(|(address, _)| *address)
        else {
            return Ok(());
        };
        let Some(mut session) = sessions.remove(&address) else {
            return Ok(());
        };
        if sleep {
            let sleep_result = session.controller.sleep(session.transport.as_mut()).await;
            if let Err(error) = sleep_result {
                sessions.insert(address, session);
                return Err(error);
            }
        }
        session.report_task.abort();
        let disconnect_result = session.transport.disconnect().await;
        let _ = events.send(ManagerEvent::Disconnected {
            controller_id: session.controller_id,
            session_id,
            reason: if sleep {
                "sleep requested"
            } else {
                "disconnected"
            }
            .into(),
            reconnecting: false,
        });
        disconnect_result
    }
}
