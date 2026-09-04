//! High-level Bluetooth discovery and controller lifecycle manager.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use polymer_command::CommandResponse;
use tokio::{
    sync::{broadcast, mpsc},
    task::JoinHandle,
    time,
};

const ADAPTER_INITIALIZATION_ATTEMPTS: usize = 3;
const ADAPTER_INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(8);
const ADAPTER_INITIALIZATION_RETRY_DELAY: Duration = Duration::from_millis(500);
const COMMAND_QUEUE_ATTEMPTS: usize = 3;
const COMMAND_QUEUE_TIMEOUT: Duration = Duration::from_millis(250);
const COMMAND_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECTION_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(55);

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
            BrEdrHid, PairingAgent, PinCode,
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
    pub vendor_product: Option<crate::devices::VendorProductId>,
    pub serial: Option<Arc<str>>,
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

polymer_command::actor_client! {
    pub client BluetoothManager(
        events: broadcast::Sender<ManagerEvent>,
    ) {
        closed: Error::ChannelClosed;
        queue_timeout:
            COMMAND_QUEUE_ATTEMPTS,
            COMMAND_QUEUE_TIMEOUT
            => Error::Timeout("Bluetooth command queue");
        response_timeout:
            COMMAND_RESPONSE_TIMEOUT
            => Error::Timeout("Bluetooth command response");

        #[args(Scan)]
        pub async fn scan(request: ScanRequest) -> Result<(), Error>;

        #[args(StopScan)]
        pub async fn stop_scan(transport: TransportKind) -> Result<(), Error>;

        #[args(Connect)]
        pub async fn connect(address: DeviceAddr, options: ConnectOptions) -> Result<(), Error>;

        #[args(SendRaw)]
        pub async fn send_raw(
            session_id: SessionId,
            bytes: impl Into<Vec<u8>>,
        ) -> Result<(), Error>;

        #[args(SetExternalOwnership)]
        pub async fn set_external_ownership(
            session_id: SessionId,
            enabled: bool,
        ) -> Result<(), Error>;

        #[args(SetPlayerIndex)]
        pub async fn set_player_index(session_id: SessionId, index: u8) -> Result<(), Error>;

        #[args(SetRumble)]
        pub async fn set_rumble(
            session_id: SessionId,
            low_frequency: u16,
            high_frequency: u16,
        ) -> Result<(), Error>;

        #[args(SetIrCamera)]
        pub async fn set_ir_camera(
            session_id: SessionId,
            setup: crate::devices::IrCameraSetup,
        ) -> Result<(), Error>;

        #[args(Sleep)]
        pub async fn sleep(session_id: SessionId) -> Result<(), Error>;

        #[args(Disconnect)]
        pub async fn disconnect(session_id: SessionId) -> Result<(), Error>;

        #[args(Forget)]
        pub async fn forget(controller_id: ControllerId) -> Result<(), Error>;

        #[args(Shutdown)]
        pub async fn shutdown() -> Result<(), Error>;
    }
}

enum Command {
    Request(BluetoothManagerCommand),
    ScanFinished {
        transport: TransportKind,
        discovered: HashMap<DeviceAddr, Discovered>,
        result: Result<()>,
        response: CommandResponse<Result<()>>,
    },
    AutoConnect {
        address: DeviceAddr,
        options: ConnectOptions,
    },
    ConnectFinished {
        address: DeviceAddr,
        transport: TransportKind,
        result: Result<Session>,
        response: Option<CommandResponse<Result<()>>>,
        reconnect: Option<ControllerRecord>,
    },
    WakeDiscovered(ControllerRecord, BleDevice),
    SessionEnded {
        controller_id: ControllerId,
        session_id: SessionId,
        reason: String,
    },
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
        Ok(Self::new_mapped(commands, Command::Request, events))
    }

    pub fn events(&self) -> broadcast::Receiver<ManagerEvent> {
        self.events.subscribe()
    }
}

#[derive(Clone)]
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
    externally_owned: bool,
}

#[derive(Clone)]
struct ConnectContext {
    config: ManagerConfig,
    host: Option<Arc<HciHost>>,
    pairing: Option<Arc<PairingAgent>>,
    keys: Arc<ControllerLinkKeyStore>,
    internal: mpsc::Sender<Command>,
    events: broadcast::Sender<ManagerEvent>,
}

struct BluetoothServer {
    config: ManagerConfig,
    commands: mpsc::Receiver<Command>,
    internal: mpsc::Sender<Command>,
    events: broadcast::Sender<ManagerEvent>,
}

struct BluetoothDriver<'state> {
    config: &'state ManagerConfig,
    bredr: &'state Option<Arc<HciHost>>,
    pairing: &'state Option<Arc<PairingAgent>>,
    keys: &'state Arc<ControllerLinkKeyStore>,
    internal: &'state mpsc::Sender<Command>,
    events: &'state broadcast::Sender<ManagerEvent>,
    discovered: &'state mut HashMap<DeviceAddr, Discovered>,
    active_scans: &'state mut HashSet<TransportKind>,
    scan_tasks: &'state mut HashMap<TransportKind, JoinHandle<()>>,
    connect_tasks: &'state mut HashMap<DeviceAddr, JoinHandle<()>>,
    connecting_transports: &'state mut HashSet<TransportKind>,
    suspended_scans: &'state mut HashSet<TransportKind>,
    next_wii_liac: &'state mut bool,
    sessions: &'state mut HashMap<DeviceAddr, Session>,
    next_session_id: &'state mut u64,
    wake_gate: &'state Arc<tokio::sync::Mutex<()>>,
    wake_tasks: &'state mut HashMap<DeviceAddr, JoinHandle<()>>,
}

#[polymer_command::command_handler]
impl BluetoothManagerCommandHandler for BluetoothDriver<'_> {
    type Output<'handler>
        = Pin<Box<dyn Future<Output = bool> + Send + 'handler>>
    where
        Self: 'handler;

    fn scan<'handler>(
        &'handler mut self,
        request: ScanRequest,
        response: CommandResponse<Result<()>>,
    ) -> Self::Output<'handler> {
        Box::pin(async move {
            let transport = match request {
                ScanRequest::Ble { .. } => TransportKind::Ble,
                ScanRequest::BrEdr { .. } => TransportKind::BrEdr,
            };
            if self.connecting_transports.contains(&transport) {
                let _ = response.send(Err(Error::Unsupported(
                    "discovery is suspended while a controller is connecting",
                )));
                return false;
            }
            if !self.active_scans.insert(transport) {
                let _ = response.send(Err(Error::Unsupported(
                    "a scan is already active on this transport",
                )));
                return false;
            }
            let _ = self.events.send(ManagerEvent::ScanState {
                transport,
                active: true,
            });
            let scan_config = self.config.clone();
            let scan_host = self.bredr.clone();
            let scan_gate = self.wake_gate.clone();
            let commands = self.internal.clone();
            let wii_liac = transport == TransportKind::BrEdr && *self.next_wii_liac;
            if transport == TransportKind::BrEdr {
                *self.next_wii_liac = !*self.next_wii_liac;
            }
            let task = tokio::spawn(async move {
                let mut found = HashMap::new();
                let result = BluetoothServer::scan(
                    &scan_config,
                    scan_host.as_ref(),
                    request,
                    wii_liac,
                    &mut found,
                    &scan_gate,
                )
                .await;
                let _ = commands
                    .send(Command::ScanFinished {
                        transport,
                        discovered: found,
                        result,
                        response,
                    })
                    .await;
            });
            self.scan_tasks.insert(transport, task);
            false
        })
    }

    async fn stop_scan(&mut self, kind: TransportKind) -> Result<()> {
        if self.suspended_scans.remove(&kind) {
            let _ = self.events.send(ManagerEvent::ScanState {
                transport: kind,
                active: false,
            });
            return Ok(());
        }
        if !self.active_scans.contains(&kind) {
            return Ok(());
        }
        match kind {
            TransportKind::Ble => stop_scan(self.config.ble_adapter_index).await,
            TransportKind::BrEdr => match self.bredr {
                Some(host) => host.cancel_inquiry().await,
                None => Err(Error::AdapterNotFound),
            },
        }
    }

    fn connect<'handler>(
        &'handler mut self,
        address: DeviceAddr,
        options: ConnectOptions,
        response: CommandResponse<Result<()>>,
    ) -> Self::Output<'handler> {
        Box::pin(async move {
            if self.sessions.contains_key(&address) {
                let _ = response.send(Ok(()));
                return false;
            }
            if self.connect_tasks.contains_key(&address) {
                let _ = response.send(Err(Error::Unsupported(
                    "a connection attempt is already active for this controller",
                )));
                return false;
            }
            let Some(device) = self.discovered.get(&address).cloned() else {
                let _ = response.send(Err(Error::Unsupported(
                    "device must be discovered before connecting",
                )));
                return false;
            };
            let transport = device.identity.transport;
            if !self.connecting_transports.insert(transport) {
                let _ = response.send(Err(Error::Unsupported(
                    "another controller connection is active on this transport",
                )));
                return false;
            }
            if self.active_scans.remove(&transport) {
                if let Some(task) = self.scan_tasks.remove(&transport) {
                    task.abort();
                    let _ = task.await;
                }
                match transport {
                    TransportKind::Ble => {
                        let _ = stop_scan(self.config.ble_adapter_index).await;
                    }
                    TransportKind::BrEdr => {
                        if let Some(host) = self.bredr {
                            let _ = host.cancel_inquiry().await;
                        }
                    }
                }
                self.suspended_scans.insert(transport);
            }
            let session_id = SessionId(*self.next_session_id);
            *self.next_session_id = self.next_session_id.wrapping_add(1).max(1);
            let task = BluetoothServer::spawn_connect(
                ConnectContext {
                    config: self.config.clone(),
                    host: self.bredr.clone(),
                    pairing: self.pairing.clone(),
                    keys: self.keys.clone(),
                    internal: self.internal.clone(),
                    events: self.events.clone(),
                },
                address,
                session_id,
                options,
                device,
                Some(response),
                None,
            );
            self.connect_tasks.insert(address, task);
            false
        })
    }

    async fn send_raw(&mut self, session_id: SessionId, bytes: Vec<u8>) -> Result<()> {
        match BluetoothServer::session_mut(self.sessions, session_id) {
            Some(session) => {
                session
                    .controller
                    .send_raw_hid(session.transport.as_mut(), &bytes)
                    .await
            }
            None => Err(Error::Unsupported("controller is not connected")),
        }
    }

    async fn set_external_ownership(&mut self, session_id: SessionId, enabled: bool) -> Result<()> {
        match BluetoothServer::session_mut(self.sessions, session_id) {
            Some(session) => {
                let result = session
                    .controller
                    .set_external_ownership(session.transport.as_mut(), enabled)
                    .await;
                if result.is_ok() {
                    session.externally_owned = enabled;
                }
                result
            }
            None => Err(Error::Unsupported("controller session is not connected")),
        }
    }

    async fn set_player_index(&mut self, session_id: SessionId, player_index: u8) -> Result<()> {
        match BluetoothServer::session_mut(self.sessions, session_id) {
            Some(session) if !session.externally_owned => {
                let result = session
                    .controller
                    .set_player_led(session.transport.as_mut(), player_index)
                    .await;
                if result.is_ok() {
                    session.player_index = player_index;
                    if let Err(error) = BluetoothServer::persist_session(self.config, session) {
                        let _ = self.events.send(ManagerEvent::Error {
                            address: Some(session.controller_id.address),
                            message: format!("failed to persist player index: {error}"),
                        });
                    }
                }
                result
            }
            Some(_) => Ok(()),
            None => Err(Error::Unsupported("controller session is not connected")),
        }
    }

    async fn set_rumble(
        &mut self,
        session_id: SessionId,
        low_frequency: u16,
        high_frequency: u16,
    ) -> Result<()> {
        match BluetoothServer::session_mut(self.sessions, session_id) {
            Some(session) if !session.externally_owned => {
                session
                    .controller
                    .set_rumble(session.transport.as_mut(), low_frequency, high_frequency)
                    .await
            }
            Some(_) => Ok(()),
            None => Err(Error::Unsupported("controller session is not connected")),
        }
    }

    async fn set_ir_camera(
        &mut self,
        session_id: SessionId,
        setup: crate::devices::IrCameraSetup,
    ) -> Result<()> {
        match BluetoothServer::session_mut(self.sessions, session_id) {
            Some(session) if !session.externally_owned => {
                session
                    .controller
                    .set_ir_camera(session.transport.as_mut(), setup)
                    .await
            }
            Some(_) => Ok(()),
            None => Err(Error::Unsupported("controller session is not connected")),
        }
    }

    async fn sleep(&mut self, session_id: SessionId) -> Result<()> {
        self.disconnect_session(session_id, true).await
    }

    async fn disconnect(&mut self, session_id: SessionId) -> Result<()> {
        self.disconnect_session(session_id, false).await
    }

    async fn forget(&mut self, controller_id: ControllerId) -> Result<()> {
        if let Some(task) = self.wake_tasks.remove(&controller_id.address) {
            task.abort();
        }
        let active_session_id = self
            .sessions
            .values()
            .find(|session| session.controller_id == controller_id)
            .map(|session| session.id);
        if let Some(session_id) = active_session_id {
            let _ =
                BluetoothServer::disconnect(session_id, false, self.events, self.sessions).await;
        }
        if let Some(pairing) = self.pairing {
            let _ = pairing.forget(controller_id.address);
        }
        self.config.store.remove(controller_id.address)
    }

    fn shutdown<'handler>(
        &'handler mut self,
        response: CommandResponse<Result<()>>,
    ) -> Self::Output<'handler> {
        Box::pin(async move {
            for (_, task) in self.scan_tasks.drain() {
                task.abort();
            }
            for (_, task) in self.wake_tasks.drain() {
                task.abort();
            }
            for (_, task) in self.connect_tasks.drain() {
                task.abort();
            }
            for (_, mut session) in self.sessions.drain() {
                let slept = match session.controller.sleep(session.transport.as_mut()).await {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::debug!(
                            %error,
                            session_id = session.id.get(),
                            "controller does not support sleeping during shutdown; disconnecting"
                        );
                        false
                    }
                };
                session.report_task.abort();
                if let Err(error) = session.transport.disconnect().await {
                    tracing::warn!(
                        %error,
                        session_id = session.id.get(),
                        "failed to disconnect controller during shutdown"
                    );
                }
                let _ = self.events.send(ManagerEvent::Disconnected {
                    controller_id: session.controller_id,
                    session_id: session.id,
                    reason: if slept {
                        "sleep requested"
                    } else {
                        "disconnected"
                    }
                    .into(),
                    reconnecting: false,
                });
            }
            let _ = response.send(Ok(()));
            true
        })
    }
}

impl BluetoothDriver<'_> {
    async fn disconnect_session(&mut self, session_id: SessionId, sleep: bool) -> Result<()> {
        let controller_id = BluetoothServer::session(self.sessions, session_id)
            .map(|session| session.controller_id);
        let result =
            BluetoothServer::disconnect(session_id, sleep, self.events, self.sessions).await;
        if sleep
            && result.is_ok()
            && let Some(controller_id) = controller_id
            && let Some(record) = self
                .config
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
            BluetoothServer::arm_ble_wake(
                record,
                self.config.controllers.clone(),
                self.internal.clone(),
                self.wake_gate.clone(),
                self.wake_tasks,
            );
        }
        result
    }
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
        let mut connect_tasks = HashMap::<DeviceAddr, JoinHandle<()>>::new();
        let mut connecting_transports = HashSet::<TransportKind>::new();
        let mut suspended_scans = HashSet::<TransportKind>::new();
        let mut next_wii_liac = true;
        let mut sessions = HashMap::<DeviceAddr, Session>::new();
        let mut next_session_id = 1_u64;
        let mut hci_events = bredr.as_ref().map(|host| host.events());
        let wake_gate = Arc::new(tokio::sync::Mutex::new(()));
        let mut wake_tasks = HashMap::<DeviceAddr, JoinHandle<()>>::new();
        for record in config.store.records().unwrap_or_default() {
            let controller = config.controllers.get(&record.profile_id);
            if controller.is_none() {
                let _ = events.send(ManagerEvent::Error {
                    address: Some(record.address),
                    message: Error::MissingProfile(record.profile_id.to_string()).to_string(),
                });
            } else if record.transport == TransportKind::Ble
                && record.auto_reconnect
                && controller
                    .is_some_and(|controller| controller.supports_transport(TransportKind::Ble))
            {
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
                            if sessions.contains_key(&addr) || connect_tasks.contains_key(&addr) { continue; }
                            let session_id = SessionId(next_session_id);
                            next_session_id = next_session_id.wrapping_add(1).max(1);
                            match Self::accept_inbound(
                                &config,
                                bredr.as_ref().unwrap(),
                                addr,
                                session_id,
                                &events,
                                internal.clone(),
                                discovered.get(&addr),
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
                Command::Request(request) => {
                    let mut driver = BluetoothDriver {
                        config: &config,
                        bredr: &bredr,
                        pairing: &pairing,
                        keys: &keys,
                        internal: &internal,
                        events: &events,
                        discovered: &mut discovered,
                        active_scans: &mut active_scans,
                        scan_tasks: &mut scan_tasks,
                        connect_tasks: &mut connect_tasks,
                        connecting_transports: &mut connecting_transports,
                        suspended_scans: &mut suspended_scans,
                        next_wii_liac: &mut next_wii_liac,
                        sessions: &mut sessions,
                        next_session_id: &mut next_session_id,
                        wake_gate: &wake_gate,
                        wake_tasks: &mut wake_tasks,
                    };
                    if request.dispatch(&mut driver).await {
                        break;
                    }
                }
                Command::ScanFinished {
                    transport,
                    discovered: found,
                    result,
                    response,
                } => {
                    if !active_scans.remove(&transport) {
                        let _ = response.send(Err(Error::Unsupported(
                            "discovery was suspended for a controller connection",
                        )));
                        continue;
                    }
                    scan_tasks.remove(&transport);
                    let mut result = result;
                    let mut recognized = Vec::new();
                    for (_, device) in found {
                        let address = device.identity.address;
                        match Self::publish_discovery(
                            &config,
                            device.identity,
                            device.bredr,
                            device.ble,
                            &events,
                            &mut discovered,
                        ) {
                            Ok(true) => recognized.push(address),
                            Ok(false) => {}
                            Err(error) if result.is_ok() => result = Err(error),
                            Err(_) => {}
                        }
                    }
                    let _ = events.send(ManagerEvent::ScanState {
                        transport,
                        active: false,
                    });
                    let _ = response.send(result);
                    for address in recognized {
                        if internal
                            .try_send(Command::AutoConnect {
                                address,
                                options: ConnectOptions::default(),
                            })
                            .is_err()
                        {
                            let _ = events.send(ManagerEvent::Error {
                                address: Some(address),
                                message: "automatic connection could not be queued".into(),
                            });
                        }
                    }
                }
                Command::AutoConnect { address, options } => {
                    let reply: Option<CommandResponse<Result<()>>> = None;
                    if sessions.contains_key(&address) {
                        if let Some(reply) = reply {
                            let _ = reply.send(Ok(()));
                        }
                        continue;
                    }
                    if connect_tasks.contains_key(&address) {
                        if let Some(reply) = reply {
                            let _ = reply.send(Err(Error::Unsupported(
                                "a connection attempt is already active for this controller",
                            )));
                        }
                        continue;
                    }
                    let Some(device) = discovered.get(&address).cloned() else {
                        if let Some(reply) = reply {
                            let _ = reply.send(Err(Error::Unsupported(
                                "device must be discovered before connecting",
                            )));
                        }
                        continue;
                    };
                    let transport = device.identity.transport;
                    if !connecting_transports.insert(transport) {
                        if let Some(reply) = reply {
                            let _ = reply.send(Err(Error::Unsupported(
                                "another controller connection is active on this transport",
                            )));
                        }
                        continue;
                    }
                    if active_scans.remove(&transport) {
                        if let Some(task) = scan_tasks.remove(&transport) {
                            task.abort();
                            let _ = task.await;
                        }
                        match transport {
                            TransportKind::Ble => {
                                let _ = stop_scan(config.ble_adapter_index).await;
                            }
                            TransportKind::BrEdr => {
                                if let Some(host) = &bredr {
                                    let _ = host.cancel_inquiry().await;
                                }
                            }
                        }
                        suspended_scans.insert(transport);
                    }
                    let session_id = SessionId(next_session_id);
                    next_session_id = next_session_id.wrapping_add(1).max(1);
                    let task = Self::spawn_connect(
                        ConnectContext {
                            config: config.clone(),
                            host: bredr.clone(),
                            pairing: pairing.clone(),
                            keys: keys.clone(),
                            internal: internal.clone(),
                            events: events.clone(),
                        },
                        address,
                        session_id,
                        options,
                        device,
                        reply,
                        None,
                    );
                    connect_tasks.insert(address, task);
                }
                Command::ConnectFinished {
                    address,
                    transport,
                    result,
                    response,
                    reconnect,
                } => {
                    connect_tasks.remove(&address);
                    connecting_transports.remove(&transport);
                    let result = match result {
                        Ok(session) => {
                            sessions.insert(address, session);
                            Ok(())
                        }
                        Err(error) => {
                            let message = error.to_string();
                            let _ = events.send(ManagerEvent::Error {
                                address: Some(address),
                                message: message.clone(),
                            });
                            if let Some(record) = reconnect {
                                Self::arm_ble_wake(
                                    record,
                                    config.controllers.clone(),
                                    internal.clone(),
                                    wake_gate.clone(),
                                    &mut wake_tasks,
                                );
                            }
                            Err(error)
                        }
                    };
                    if let Some(response) = response {
                        let _ = response.send(result);
                    }
                    if suspended_scans.remove(&transport) {
                        let _ = events.send(ManagerEvent::ScanState {
                            transport,
                            active: false,
                        });
                    }
                }
                Command::WakeDiscovered(record, device) => {
                    wake_tasks.remove(&record.address);
                    let identity = Self::ble_identity(&device);
                    let recognized = match Self::publish_discovery(
                        &config,
                        identity,
                        None,
                        Some(device),
                        &events,
                        &mut discovered,
                    ) {
                        Ok(recognized) => recognized,
                        Err(error) => {
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
                            continue;
                        }
                    };
                    let options = ConnectOptions {
                        player_index: record.player_index,
                        auto_reconnect: true,
                    };
                    if !recognized {
                        let _ = events.send(ManagerEvent::Error {
                            address: Some(record.address),
                            message: "automatic reconnect advertisement does not match a BLE-capable controller profile".into(),
                        });
                    } else if sessions.contains_key(&record.address)
                        || connect_tasks.contains_key(&record.address)
                    {
                        Self::arm_ble_wake(
                            record,
                            config.controllers.clone(),
                            internal.clone(),
                            wake_gate.clone(),
                            &mut wake_tasks,
                        );
                    } else if let Some(device) = discovered.get(&record.address).cloned() {
                        let address = record.address;
                        let transport = device.identity.transport;
                        if !connecting_transports.insert(transport) {
                            Self::arm_ble_wake(
                                record,
                                config.controllers.clone(),
                                internal.clone(),
                                wake_gate.clone(),
                                &mut wake_tasks,
                            );
                            continue;
                        }
                        let task = Self::spawn_connect(
                            ConnectContext {
                                config: config.clone(),
                                host: bredr.clone(),
                                pairing: pairing.clone(),
                                keys: keys.clone(),
                                internal: internal.clone(),
                                events: events.clone(),
                            },
                            address,
                            SessionId(next_session_id),
                            options,
                            device,
                            None,
                            Some(record),
                        );
                        connect_tasks.insert(address, task);
                        next_session_id = next_session_id.wrapping_add(1).max(1);
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
        wii_liac: bool,
        discovered: &mut HashMap<DeviceAddr, Discovered>,
        wake_gate: &Arc<tokio::sync::Mutex<()>>,
    ) -> Result<()> {
        match request {
            ScanRequest::Ble { duration } => {
                let devices = {
                    let _guard = wake_gate.lock().await;
                    scan_ble(config.ble_adapter_index, duration).await?
                };
                for device in devices {
                    let identity = Self::ble_identity(&device);
                    discovered.insert(
                        identity.address,
                        Discovered {
                            identity,
                            bredr: None,
                            ble: Some(device),
                        },
                    );
                }
                Ok(())
            }
            ScanRequest::BrEdr { duration_units } => {
                let host = bredr.ok_or(Error::AdapterNotFound)?;
                let inquiries = if wii_liac {
                    host.limited_inquiry(duration_units, 0).await?
                } else {
                    host.inquiry(duration_units, 0).await?
                };
                for inquiry in inquiries {
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
                    discovered.insert(
                        identity.address,
                        Discovered {
                            identity,
                            bredr: Some(inquiry),
                            ble: None,
                        },
                    );
                }
                Ok(())
            }
        }
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
    ) -> Result<bool> {
        let profile_id = config
            .controllers
            .identify(&identity)?
            .map(|controller| controller.id());
        let recognized = profile_id.is_some();
        let rssi = ble
            .as_ref()
            .and_then(|device| device.rssi)
            .or_else(|| bredr.as_ref().and_then(|device| device.rssi).map(i16::from));
        let event = ManagerEvent::Discovered {
            identity: identity.clone(),
            profile_id,
            rssi,
        };
        let replace = recognized
            || discovered.get(&identity.address).is_none_or(|existing| {
                config
                    .controllers
                    .identify(&existing.identity)
                    .ok()
                    .flatten()
                    .is_none()
            });
        if replace {
            discovered.insert(
                identity.address,
                Discovered {
                    identity,
                    bredr,
                    ble,
                },
            );
        }
        let _ = events.send(event);
        Ok(recognized)
    }

    fn spawn_connect(
        context: ConnectContext,
        address: DeviceAddr,
        session_id: SessionId,
        options: ConnectOptions,
        device: Discovered,
        response: Option<CommandResponse<Result<()>>>,
        reconnect: Option<ControllerRecord>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let transport = device.identity.transport;
            let completion = context.internal.clone();
            let result = time::timeout(
                CONNECTION_ATTEMPT_TIMEOUT,
                Self::connect(context, address, session_id, options, device),
            )
            .await
            .unwrap_or(Err(Error::Timeout("controller connection")));
            let _ = completion
                .send(Command::ConnectFinished {
                    address,
                    transport,
                    result,
                    response,
                    reconnect,
                })
                .await;
        })
    }

    async fn connect(
        context: ConnectContext,
        address: DeviceAddr,
        session_id: SessionId,
        options: ConnectOptions,
        device: Discovered,
    ) -> Result<Session> {
        let ConnectContext {
            config,
            host,
            pairing,
            keys,
            internal,
            events,
        } = context;
        let mut identity = device.identity.clone();
        let mut identified_handle = None;
        if identity.transport == TransportKind::BrEdr {
            let preliminary = config.controllers.identify(&identity)?;
            if preliminary
                .as_ref()
                .is_none_or(|controller| controller.id().as_str() == "generic.hid")
            {
                let host = host.as_ref().ok_or(Error::AdapterNotFound)?;
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
        let mut controller = config
            .controllers
            .identify(&identity)?
            .ok_or(Error::Unsupported(
                "no controller profile matched the device",
            ))?;
        keys.register(ControllerRecord {
            address,
            transport: identity.transport,
            profile_id: controller.id(),
            link_key: None,
            link_key_type: None,
            adapter_index: (identity.transport == TransportKind::Ble)
                .then_some(config.ble_adapter_index),
            auto_reconnect: options.auto_reconnect,
            player_index: options.player_index,
        })?;
        let controller_id = ControllerId::new(identity.transport, address);
        let _ = events.send(ManagerEvent::Connecting {
            controller_id,
            profile_id: controller.id(),
        });
        let policy = controller.connection_policy(&identity, &options);
        let mut transport: Box<dyn Transport> = match device.identity.transport {
            TransportKind::BrEdr => {
                let host = host.as_ref().ok_or(Error::AdapterNotFound)?;
                let inquiry = device
                    .bredr
                    .as_ref()
                    .ok_or(Error::InvalidPacket("missing BR/EDR inquiry data"))?;
                host.set_host_identity(
                    policy.host_identity.name,
                    policy.host_identity.class_of_device,
                )
                .await?;
                if let Some(pairing) = pairing.as_ref() {
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
                let restore_result = host.set_host_identity("Polymer", [0x0c, 0x01, 0x1c]).await;
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
        if let Some(uuid) = transport.input_characteristic_uuid()
            && let Some(refined) = config.controllers.identify_input_characteristic(uuid)?
        {
            controller = refined;
        }
        identity
            .name
            .get_or_insert_with(|| controller.display_name().to_owned());
        let profile_id = controller.id();
        let vendor_product = controller.vendor_product(&identity);
        let mut controller_session = controller.connect(&options);
        controller_session.synchronize(transport.as_mut()).await?;
        let serial = controller_session
            .serial()
            .or_else(|| controller.serial(&identity));
        keys.register(ControllerRecord {
            address,
            transport: identity.transport,
            profile_id: profile_id.clone(),
            link_key: None,
            link_key_type: None,
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
        let session = Session {
            id: session_id,
            controller_id,
            identity: identity.clone(),
            player_index: options.player_index,
            transport,
            controller: controller_session,
            report_task,
            externally_owned: false,
        };
        let _ = events.send(ManagerEvent::Connected(ConnectedController {
            controller_id,
            session_id,
            identity,
            profile_id,
            vendor_product,
            serial,
            player_index: options.player_index,
        }));
        Ok(session)
    }

    async fn accept_inbound(
        config: &ManagerConfig,
        host: &Arc<HciHost>,
        address: DeviceAddr,
        session_id: SessionId,
        events: &broadcast::Sender<ManagerEvent>,
        internal: mpsc::Sender<Command>,
        discovered: Option<&Discovered>,
    ) -> Result<Session> {
        let mut record = config
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
        let controller = match config.controllers.get(&record.profile_id) {
            Some(controller) => controller,
            None => {
                let identity = &discovered
                    .ok_or_else(|| Error::MissingProfile(record.profile_id.to_string()))?
                    .identity;
                let controller = config
                    .controllers
                    .identify(identity)?
                    .ok_or_else(|| Error::MissingProfile(record.profile_id.to_string()))?;
                record.profile_id = controller.id();
                config.store.put(record.clone())?;
                controller
            }
        };
        let identity = discovered.map_or_else(
            || DeviceIdentity {
                address,
                transport: TransportKind::BrEdr,
                name: None,
                class_of_device: None,
                service_uuids: Vec::new(),
                manufacturer_data: Vec::new(),
                pnp: None,
            },
            |device| device.identity.clone(),
        );
        let options = ConnectOptions {
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
        let restore_result = host.set_host_identity("Polymer", [0x0c, 0x01, 0x1c]).await;
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
        let vendor_product = controller.vendor_product(&identity);
        let mut controller_session = controller.connect(&options);
        if let Err(error) = controller_session.synchronize(transport.as_mut()).await {
            let _ = transport.disconnect().await;
            return Err(error);
        }
        let serial = controller_session
            .serial()
            .or_else(|| controller.serial(&identity));
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
            vendor_product,
            serial,
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
            externally_owned: false,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn wiimote_identity(address: DeviceAddr, transport: TransportKind) -> DeviceIdentity {
        DeviceIdentity {
            address,
            transport,
            name: Some("Nintendo RVL-CNT-01".to_owned()),
            class_of_device: None,
            service_uuids: Vec::new(),
            manufacturer_data: Vec::new(),
            pnp: None,
        }
    }

    #[test]
    fn unrecognized_ble_observation_does_not_replace_classic_controller() {
        let config = ManagerConfig::default();
        let (events, _) = broadcast::channel(8);
        let address = DeviceAddr::public([1, 2, 3, 4, 5, 6]);
        let mut discovered = HashMap::new();

        assert!(
            BluetoothServer::publish_discovery(
                &config,
                wiimote_identity(address, TransportKind::BrEdr),
                None,
                None,
                &events,
                &mut discovered,
            )
            .unwrap()
        );
        assert!(
            !BluetoothServer::publish_discovery(
                &config,
                wiimote_identity(address, TransportKind::Ble),
                None,
                None,
                &events,
                &mut discovered,
            )
            .unwrap()
        );
        assert_eq!(
            discovered.get(&address).unwrap().identity.transport,
            TransportKind::BrEdr
        );
    }
}
