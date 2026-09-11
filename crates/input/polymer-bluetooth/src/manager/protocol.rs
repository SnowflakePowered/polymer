use super::*;

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
pub struct SessionId(pub(super) u64);

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

pub(super) enum Command {
    HciEvent(std::result::Result<HciEvent, broadcast::error::RecvError>),
    ScanFinished {
        operation_id: SessionId,
        transport: TransportKind,
        discovered: HashMap<DeviceAddr, Discovered>,
        result: Result<()>,
        response: ReplySender<Result<()>>,
    },
    AutoConnect {
        address: DeviceAddr,
        options: ConnectOptions,
    },
    ConnectFinished {
        session_id: SessionId,
        address: DeviceAddr,
        transport: TransportKind,
        result: Result<Session>,
        response: Option<ReplySender<Result<()>>>,
        reconnect: Option<ControllerRecord>,
    },
    WakeDiscovered(ControllerRecord, BleDevice, tokio::task::Id),
    SessionEnded {
        controller_id: ControllerId,
        session_id: SessionId,
        reason: String,
    },
}

impl BluetoothManager {
    pub async fn start(config: ManagerConfig) -> Result<Self> {
        let (events, _) = broadcast::channel(1024);
        let policy = Admission::new(64);
        let actor = BluetoothActor::spawn_with_mailbox(
            (config, events.clone(), policy.clone()),
            kameo::mailbox::unbounded(),
        );
        Ok(Self::from_actor(actor, policy, events))
    }

    pub fn events(&self) -> broadcast::Receiver<ManagerEvent> {
        self.events.subscribe()
    }
}
