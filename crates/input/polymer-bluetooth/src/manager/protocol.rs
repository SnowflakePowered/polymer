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

/// Actor access and event subscription; requests use Kameo directly.
#[derive(Clone, Debug)]
pub struct BluetoothManager {
    pub actor: ActorRef<BluetoothActor>,
    events: broadcast::Sender<ManagerEvent>,
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
        let prepared = BluetoothActor::prepare_with_mailbox(kameo::mailbox::bounded(64))
            .reply_timeout(COMMAND_RESPONSE_TIMEOUT);
        let actor = prepared.actor_ref().clone();
        prepared.spawn((config, events.clone()));
        Ok(Self { actor, events })
    }

    pub fn events(&self) -> broadcast::Receiver<ManagerEvent> {
        self.events.subscribe()
    }
}
