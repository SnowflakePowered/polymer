use super::*;

pub(super) enum ActiveTransport {
    BrEdr(BrEdrHid),
    Ble(Box<BleGatt>),
}

impl ActiveTransport {
    pub(super) async fn send(&mut self, bytes: &[u8]) -> polymer_bluetooth::Result<()> {
        match self {
            Self::BrEdr(transport) => transport.send(bytes).await,
            Self::Ble(_) => Err(polymer_bluetooth::Error::Unsupported(
                "arbitrary Switch 2 BLE writes are disabled for controller safety",
            )),
        }
    }

    pub(super) async fn disconnect(&mut self) -> polymer_bluetooth::Result<()> {
        match self {
            Self::BrEdr(transport) => transport.disconnect().await,
            Self::Ble(transport) => transport.disconnect().await,
        }
    }

    pub(super) fn bredr_handle(&self) -> Option<u16> {
        match self {
            Self::BrEdr(transport) => transport.handle(),
            Self::Ble(_) => None,
        }
    }
}

pub(super) struct ActiveController {
    pub(super) transport: ActiveTransport,
    pub(super) report_task: JoinHandle<()>,
    pub(super) identity: BrIdentity,
    pub(super) slot: usize,
    pub(super) session: Option<Box<dyn ControllerSession>>,
}

impl ActiveController {
    pub(super) async fn sleep(&mut self) -> polymer_bluetooth::Result<()> {
        let Some(session) = &mut self.session else {
            return Ok(());
        };
        match &mut self.transport {
            ActiveTransport::BrEdr(transport) => session.sleep(transport).await,
            ActiveTransport::Ble(transport) => session.sleep(transport.as_mut()).await,
        }
    }
}

pub(super) struct PendingInbound {
    pub(super) handle: u16,
    pub(super) link: Option<u64>,
    pub(super) slot: usize,
    pub(super) task: JoinHandle<()>,
}

pub(super) struct PendingOutbound {
    pub(super) slot: usize,
    pub(super) task: JoinHandle<()>,
}

pub(super) struct BrConnection {
    pub(super) transport: BrEdrHid,
    pub(super) reports: ReportReceiver,
    pub(super) session: Option<Box<dyn ControllerSession>>,
}

pub(super) struct InboundResult {
    pub(super) addr: DeviceAddr,
    pub(super) handle: u16,
    pub(super) identity: BrIdentity,
    pub(super) slot: usize,
    pub(super) result: polymer_bluetooth::Result<BrConnection>,
}

pub(super) struct OutboundResult {
    pub(super) addr: DeviceAddr,
    pub(super) identity: BrIdentity,
    pub(super) slot: usize,
    pub(super) result: polymer_bluetooth::Result<BrConnection>,
}

pub(super) struct PendingBle {
    pub(super) slot: usize,
    pub(super) task: JoinHandle<()>,
}

pub(super) struct BleConnection {
    pub(super) transport: BleGatt,
    pub(super) notifications: GattNotificationReceiver,
    pub(super) disconnects: tokio::sync::broadcast::Receiver<()>,
    pub(super) session: Box<dyn ControllerSession>,
}

pub(super) struct BleResult {
    pub(super) addr: DeviceAddr,
    pub(super) slot: usize,
    pub(super) adapter_index: usize,
    pub(super) device: BleDevice,
    pub(super) profile_id: ProfileId,
    pub(super) automatic: bool,
    pub(super) result: polymer_bluetooth::Result<BleConnection>,
}

#[derive(Clone)]
pub(super) struct KnownBle {
    pub(super) adapter_index: usize,
    pub(super) device: BleDevice,
    pub(super) profile_id: ProfileId,
    pub(super) slot: usize,
}

impl KnownBle {
    pub(super) fn controller(&self) -> Option<Arc<dyn ControllerDevice>> {
        ControllerRegistry::standard().get(&self.profile_id)
    }

    pub(super) fn label(&self) -> &'static str {
        self.controller()
            .map_or("Switch 2 controller", |controller| {
                controller.display_name()
            })
    }
}

pub(super) struct BleDisconnected {
    pub(super) addr: DeviceAddr,
    pub(super) slot: usize,
}

pub(super) struct BleRecovery {
    pub(super) adapter_index: usize,
    pub(super) device: BleDevice,
}

#[derive(Clone, Debug, Default)]
pub(super) struct BrIdentity {
    pub(super) profile_id: Option<ProfileId>,
}

impl BrIdentity {
    pub(super) fn new(profile_id: Option<ProfileId>) -> Self {
        Self { profile_id }
    }

    pub(super) fn token(&self) -> Option<&str> {
        self.profile_id.as_ref().map(ProfileId::as_str)
    }

    pub(super) fn label(&self) -> &'static str {
        self.controller()
            .map_or("BR/EDR controller", |controller| controller.display_name())
    }

    fn from_token(token: &str) -> Option<Self> {
        let profile_id = ProfileId::new(token);
        ControllerRegistry::standard()
            .get(&profile_id)
            .map(|_| Self::new(Some(profile_id)))
    }

    pub(super) fn controller(&self) -> Option<Arc<dyn ControllerDevice>> {
        ControllerRegistry::standard().get(self.profile_id.as_ref()?)
    }

    pub(super) fn from_profile_id(id: &str) -> Option<Self> {
        Self::from_token(id)
    }

    pub(super) fn is_wii_remote(&self) -> bool {
        self.token()
            .is_some_and(|id| id.starts_with("nintendo.wiimote."))
    }

    pub(super) fn is_switch(&self) -> bool {
        self.token()
            .is_some_and(|id| id.starts_with("nintendo.switch."))
    }

    pub(super) async fn connect(
        &self,
        transport: &mut dyn Transport,
        slot: usize,
    ) -> polymer_bluetooth::Result<Option<Box<dyn ControllerSession>>> {
        let Some(controller) = self.controller() else {
            return Ok(None);
        };
        let mut session = controller.connect(&connect_options(slot));
        session.synchronize(transport).await?;
        Ok(Some(session))
    }
}

pub(super) fn ble_device_identity(device: &BleDevice) -> DeviceIdentity {
    DeviceIdentity {
        address: device.addr,
        transport: TransportKind::Ble,
        name: device.name.clone(),
        class_of_device: None,
        service_uuids: device.services.clone(),
        manufacturer_data: device
            .manufacturer_data
            .iter()
            .map(|(id, data)| (*id, data.clone()))
            .collect(),
        pnp: None,
    }
}

pub(super) fn controller_for_ble(device: &BleDevice) -> Option<Arc<dyn ControllerDevice>> {
    ControllerRegistry::standard()
        .identify(&ble_device_identity(device))
        .ok()
        .flatten()
}
