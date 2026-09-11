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
    pub ds3_motion: Option<crate::devices::sony::dualshock3::motion::MotionState>,
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

/// Actor access and event subscription; requests use Actix directly.
#[derive(Clone, Debug)]
pub struct BluetoothManager {
    pub actor: Addr<BluetoothActor>,
    events: broadcast::Sender<ManagerEvent>,
}

#[derive(actix::Message)]
#[rtype(result = "()")]
pub(super) struct HciLifecycle(pub std::result::Result<HciEvent, broadcast::error::RecvError>);

#[derive(actix::Message)]
#[rtype(result = "()")]
pub(super) struct WakeDiscovered(pub ControllerRecord, pub BleDevice, pub tokio::task::Id);

#[derive(actix::Message)]
#[rtype(result = "()")]
pub(super) struct SessionEnded {
    pub controller_id: ControllerId,
    pub session_id: SessionId,
    pub reason: String,
}

impl BluetoothManager {
    pub async fn start(config: ManagerConfig) -> Result<Self> {
        let (events, _) = broadcast::channel(1024);
        let (ready, startup) = tokio::sync::oneshot::channel();
        let actor_events = events.clone();
        std::thread::Builder::new()
            .name("polymer-bluetooth".to_owned())
            .spawn(move || {
                let system = System::new();
                let actor = system.block_on(async move {
                    BluetoothActor::create(move |ctx| {
                        ctx.set_mailbox_capacity(0);
                        BluetoothActor::new(config, actor_events, ctx.address().downgrade())
                    })
                });
                if ready.send(actor).is_ok()
                    && let Err(error) = system.run()
                {
                    tracing::error!(%error, "Bluetooth actor system failed");
                }
            })
            .map_err(Error::Io)?;
        let actor = startup.await.map_err(|_| Error::ChannelClosed)?;
        Ok(Self { actor, events })
    }

    pub fn events(&self) -> broadcast::Receiver<ManagerEvent> {
        self.events.subscribe()
    }
}
