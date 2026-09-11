use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use crate::input::{decoder, wiimote};
use polymer_bluetooth::{
    DeviceAddr, ReportReceiver, Transport, TransportKind,
    devices::{
        ConnectOptions, ControllerDevice, ControllerRegistry, ControllerSession, DeviceIdentity,
        ProfileId,
        nintendo::switch2::{COMMON_INPUT_UUID, UnknownSwitch2Controller},
    },
    store::{
        ControllerLinkKeyStore, ControllerRecord, ControllerStore, MemoryControllerStore,
        SqliteControllerStore,
    },
    transport::{
        ble::{
            BleDevice, BleGatt, GattNotificationReceiver, default_adapter_address,
            scan as scan_ble, stop_scan as stop_ble_adapter_scan,
        },
        bredr::{
            BrEdrHid, PairingAgent, PinCode,
            hci::{AdapterSelector, Event as HciEvent, HciHost, InquiryResult},
        },
    },
};
use tokio::{
    sync::{Mutex, mpsc as tokio_mpsc},
    task::JoinHandle,
};

mod identity;

fn connect_options(slot: usize) -> ConnectOptions {
    ConnectOptions {
        player_index: u8::try_from(slot).unwrap_or(u8::MAX),
        ..ConnectOptions::default()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BondingMode {
    Ssp,
    WiiOneTwo,
    WiiSync,
}

impl BondingMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ssp => "SSP / automatic",
            Self::WiiOneTwo => "Wii Remote 1+2",
            Self::WiiSync => "Wii Remote red SYNC",
        }
    }
    pub fn help(self) -> &'static str {
        match self {
            Self::Ssp => {
                "DS4, DualSense, Switch 1 controllers, and other SSP devices. Nintendo uses 0000 if it requests a legacy PIN."
            }
            Self::WiiOneTwo => {
                "Temporary Wii sync: PIN is the Wii Remote address in reverse byte order."
            }
            Self::WiiSync => {
                "Persistent Wii sync: PIN is the dedicated adapter address in reverse byte order."
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct DeviceView {
    pub inquiry: InquiryResult,
    pub addr: String,
    pub name: String,
    pub rssi: Option<i8>,
    pub class: String,
    pub profile_id: Option<ProfileId>,
    pub profile_name: Option<&'static str>,
}

impl DeviceView {
    pub fn is_wii_remote(&self) -> bool {
        self.profile_id
            .as_ref()
            .is_some_and(|id| id.as_str().starts_with("nintendo.wiimote."))
    }

    pub fn is_switch(&self) -> bool {
        self.profile_id
            .as_ref()
            .is_some_and(|id| id.as_str().starts_with("nintendo.switch."))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioKind {
    BrEdr,
    Ble,
}

pub enum Command {
    CablePairing(crate::cable_pairing::Action),
    BeginControllerOrder {
        adapter_index: usize,
    },
    EndControllerOrder,
    ConnectBr {
        device: InquiryResult,
        pin: BondingMode,
        profile_id: Option<ProfileId>,
    },
    ConnectBle {
        adapter_index: usize,
        device: BleDevice,
    },
    SelectSwitch2InputReport {
        addr: DeviceAddr,
        report_id: u8,
    },
    SubscribeAllSwitch2InputReports {
        addr: DeviceAddr,
    },
    Forget(DeviceAddr),
    ListenInbound(bool),
    SendOutput {
        addr: DeviceAddr,
        bytes: Vec<u8>,
    },
    SleepBle(DeviceAddr),
    Disconnect(Option<DeviceAddr>),
    Shutdown,
}

pub enum Event {
    CablePairing(crate::cable_pairing::Event),
    BrReady(String),
    BleReady(Vec<String>),
    Scanning {
        radio: RadioKind,
        enabled: bool,
        message: String,
    },
    Busy(String),
    Idle(String),
    Device(DeviceView),
    BleDevice(BleDevice),
    Connected {
        addr: String,
        name: String,
        radio: RadioKind,
        slot: usize,
    },
    Disconnected {
        addr: String,
        reason: String,
    },
    Report {
        addr: String,
        bytes: Vec<u8>,
    },
    BleReport {
        addr: String,
        characteristic: String,
        bytes: Vec<u8>,
        received_at: Instant,
    },
    BleReportsDropped {
        addr: String,
        count: u64,
    },
    Switch2InputReportSelected {
        addr: String,
        report_id: u8,
    },
    Switch2InputReportSelectionFailed {
        addr: String,
        report_id: u8,
        reason: String,
    },
    Switch2InputReportsSubscribed {
        addr: String,
        report_ids: Vec<u8>,
    },
    Switch2InputReportsSubscriptionFailed {
        addr: String,
        reason: String,
    },
    WiiTelemetry {
        addr: String,
        telemetry: wiimote::Telemetry,
        received_at: Instant,
    },
    DecodedInput {
        addr: String,
        input: decoder::DecodedInput,
    },
    Log(String),
    Error(String),
}

enum ActiveTransport {
    BrEdr(BrEdrHid),
    Ble(Box<BleGatt>),
}

impl ActiveTransport {
    async fn send(&mut self, bytes: &[u8]) -> polymer_bluetooth::Result<()> {
        match self {
            Self::BrEdr(transport) => transport.send(bytes).await,
            Self::Ble(_) => Err(polymer_bluetooth::Error::Unsupported(
                "arbitrary Switch 2 BLE writes are disabled for controller safety",
            )),
        }
    }

    async fn disconnect(&mut self) -> polymer_bluetooth::Result<()> {
        match self {
            Self::BrEdr(transport) => transport.disconnect().await,
            Self::Ble(transport) => transport.disconnect().await,
        }
    }

    fn bredr_handle(&self) -> Option<u16> {
        match self {
            Self::BrEdr(transport) => transport.handle(),
            Self::Ble(_) => None,
        }
    }
}

struct ActiveController {
    transport: ActiveTransport,
    report_task: JoinHandle<()>,
    identity: BrIdentity,
    slot: usize,
    session: Option<Box<dyn ControllerSession>>,
}

impl ActiveController {
    async fn sleep(&mut self) -> polymer_bluetooth::Result<()> {
        let Some(session) = &mut self.session else {
            return Ok(());
        };
        match &mut self.transport {
            ActiveTransport::BrEdr(transport) => session.sleep(transport).await,
            ActiveTransport::Ble(transport) => session.sleep(transport.as_mut()).await,
        }
    }
}

struct PendingInbound {
    handle: u16,
    slot: usize,
    task: JoinHandle<()>,
}

struct PendingOutbound {
    slot: usize,
    task: JoinHandle<()>,
}

struct BrConnection {
    transport: BrEdrHid,
    reports: ReportReceiver,
    session: Option<Box<dyn ControllerSession>>,
}

struct InboundResult {
    addr: DeviceAddr,
    handle: u16,
    identity: BrIdentity,
    slot: usize,
    result: polymer_bluetooth::Result<BrConnection>,
}

struct OutboundResult {
    addr: DeviceAddr,
    identity: BrIdentity,
    slot: usize,
    result: polymer_bluetooth::Result<BrConnection>,
}

struct PendingBle {
    slot: usize,
    task: JoinHandle<()>,
}

struct BleConnection {
    transport: BleGatt,
    notifications: GattNotificationReceiver,
    disconnects: tokio::sync::broadcast::Receiver<()>,
    session: Box<dyn ControllerSession>,
}

struct BleResult {
    addr: DeviceAddr,
    slot: usize,
    adapter_index: usize,
    device: BleDevice,
    profile_id: ProfileId,
    automatic: bool,
    result: polymer_bluetooth::Result<BleConnection>,
}

#[derive(Clone)]
struct KnownBle {
    adapter_index: usize,
    device: BleDevice,
    profile_id: ProfileId,
    slot: usize,
}

impl KnownBle {
    fn controller(&self) -> Option<Arc<dyn ControllerDevice>> {
        ControllerRegistry::standard().get(&self.profile_id)
    }

    fn label(&self) -> &'static str {
        self.controller()
            .map_or("Switch 2 controller", |controller| {
                controller.display_name()
            })
    }
}

struct BleDisconnected {
    addr: DeviceAddr,
    slot: usize,
}

struct BleRecovery {
    adapter_index: usize,
    device: BleDevice,
}

#[derive(Clone, Debug, Default)]
struct BrIdentity {
    profile_id: Option<ProfileId>,
}

impl BrIdentity {
    fn new(profile_id: Option<ProfileId>) -> Self {
        Self { profile_id }
    }

    fn token(&self) -> Option<&str> {
        self.profile_id.as_ref().map(ProfileId::as_str)
    }

    fn label(&self) -> &'static str {
        self.controller()
            .map_or("BR/EDR controller", |controller| controller.display_name())
    }

    fn from_token(token: &str) -> Option<Self> {
        let profile_id = ProfileId::new(token);
        ControllerRegistry::standard()
            .get(&profile_id)
            .map(|_| Self::new(Some(profile_id)))
    }

    fn controller(&self) -> Option<Arc<dyn ControllerDevice>> {
        ControllerRegistry::standard().get(self.profile_id.as_ref()?)
    }

    fn from_profile_id(id: &str) -> Option<Self> {
        Self::from_token(id)
    }

    fn is_wii_remote(&self) -> bool {
        self.token()
            .is_some_and(|id| id.starts_with("nintendo.wiimote."))
    }

    fn is_switch(&self) -> bool {
        self.token()
            .is_some_and(|id| id.starts_with("nintendo.switch."))
    }

    async fn connect(
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

fn ble_device_identity(device: &BleDevice) -> DeviceIdentity {
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

fn controller_for_ble(device: &BleDevice) -> Option<Arc<dyn ControllerDevice>> {
    ControllerRegistry::standard()
        .identify(&ble_device_identity(device))
        .ok()
        .flatten()
}

pub fn spawn() -> (tokio_mpsc::UnboundedSender<Command>, mpsc::Receiver<Event>) {
    reset_diagnostic_log();
    diagnostic("Starting Polymer Bluetooth test worker");
    let (command_tx, command_rx) = tokio_mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::channel();
    thread::Builder::new()
        .name("polymer-bttest".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(2)
                .build();
            match runtime {
                Ok(runtime) => runtime.block_on(run(command_rx, event_tx)),
                Err(error) => {
                    let _ = event_tx.send(Event::Error(format!(
                        "could not start async runtime: {error}"
                    )));
                }
            }
        })
        .expect("failed to spawn Bluetooth test worker");
    (command_tx, event_rx)
}

async fn run(mut commands: tokio_mpsc::UnboundedReceiver<Command>, events: mpsc::Sender<Event>) {
    let (controller_store, cable_registry_error): (Arc<dyn ControllerStore>, Option<String>) =
        match polymer_library::Directories::discover()
            .map_err(|error| error.to_string())
            .and_then(|directories| {
                SqliteControllerStore::open(directories.storage().join("input.db"))
                    .map_err(|error| error.to_string())
            }) {
            Ok(store) => (Arc::new(store), None),
            Err(error) => {
                let _ = events.send(Event::Log(format!(
                    "controller registry unavailable; using memory only: {error}"
                )));
                let _ = events.send(Event::CablePairing(
                    crate::cable_pairing::Event::RegistryUnavailable(error.clone()),
                ));
                (Arc::new(MemoryControllerStore::default()), Some(error))
            }
        };
    let ble_adapter_count = match BleGatt::adapter_names().await {
        Ok(names) if !names.is_empty() => {
            let count = names.len();
            diagnostic(&format!("OS BLE adapters: {}", names.join(", ")));
            let _ = events.send(Event::BleReady(names));
            count
        }
        Ok(_) => {
            let _ = events.send(Event::Log("No OS-owned BLE adapter found".into()));
            0
        }
        Err(error) => {
            diagnostic(&format!("BLE adapter error: {error}"));
            let _ = events.send(Event::Log(format!("BLE adapter error: {error}")));
            0
        }
    };

    let selector = AdapterSelector::first_winusb();
    let host = match HciHost::open(&selector).await {
        Ok(host) => Some(Arc::new(host)),
        Err(error) => {
            diagnostic(&format!("Dedicated BR/EDR adapter unavailable: {error}"));
            let _ = events.send(Event::Log(format!(
                "Dedicated BR/EDR adapter unavailable: {error}"
            )));
            None
        }
    };
    let pairing = host.as_ref().map(|host| {
        Arc::new(PairingAgent::new(
            host.clone(),
            Arc::new(ControllerLinkKeyStore::new(controller_store.clone())),
        ))
    });
    let pairing_task = pairing.as_ref().map(|pairing| {
        let pairing = pairing.clone();
        let pairing_events = events.clone();
        tokio::spawn(async move {
            if let Err(error) = pairing.run().await {
                let _ = pairing_events.send(Event::Error(format!(
                    "BR/EDR pairing agent stopped: {error}"
                )));
            }
        })
    });
    if let Some(host) = &host {
        let info = host.info();
        let _ = events.send(Event::CablePairing(crate::cable_pairing::Event::Host(
            polymer_bluetooth::cable_pairing::CablePairingHost {
                address: info.address,
                adapter_index: None,
            },
        )));
        diagnostic(&format!(
            "BR/EDR adapter ready: {} (HCI {} rev {:04X}, LMP {} subver {:04X}, manufacturer {}; ACL {} bytes / {} buffers)",
            info.address,
            info.hci_version,
            info.hci_revision,
            info.lmp_version,
            info.lmp_subversion,
            info.manufacturer,
            info.acl_data_packet_length,
            info.total_acl_packets
        ));
        if let Some(source) = &info.firmware_source {
            diagnostic(&format!("Loaded adapter firmware from {source}"));
            let _ = events.send(Event::Log(format!(
                "Initialized dedicated radio firmware from {source}"
            )));
        }
        let _ = events.send(Event::BrReady(format!(
            "WinUSB {:04X}:{:04X} {} · ACL {} bytes / {} buffers",
            info.usb_vendor_id,
            info.usb_product_id,
            info.address,
            info.acl_data_packet_length,
            info.total_acl_packets
        )));
    }

    let mut hci_events = host.as_ref().map(|host| host.events());
    let mut active: HashMap<DeviceAddr, ActiveController> = HashMap::new();
    let mut pending_inbound: HashMap<DeviceAddr, PendingInbound> = HashMap::new();
    let (inbound_tx, mut inbound_rx) = tokio_mpsc::unbounded_channel::<InboundResult>();
    let mut pending_outbound: HashMap<DeviceAddr, PendingOutbound> = HashMap::new();
    let (outbound_tx, mut outbound_rx) = tokio_mpsc::unbounded_channel::<OutboundResult>();
    let mut pending_ble: HashMap<DeviceAddr, PendingBle> = HashMap::new();
    let (ble_tx, mut ble_rx) = tokio_mpsc::unbounded_channel::<BleResult>();
    let (ble_disconnect_tx, mut ble_disconnect_rx) =
        tokio_mpsc::unbounded_channel::<BleDisconnected>();
    let (ble_recovery_tx, mut ble_recovery_rx) = tokio_mpsc::unbounded_channel::<BleRecovery>();
    // WinRT exposes scanning as adapter-wide state. Every discovery path must
    // share this gate or one controller's stop_scan cancels another's wake.
    let ble_scan_gate = Arc::new(Mutex::new(()));
    let mut known_ble = load_known_ble_controllers(controller_store.as_ref());
    let mut restored_ble = known_ble.values().cloned().collect::<Vec<_>>();
    restored_ble.sort_by_key(|known| known.slot);
    for known in restored_ble {
        let addr = known.device.addr;
        diagnostic(&format!(
            "Restored bonded Switch 2 controller {addr} ({}) in player slot {}; waiting for wake advertisement",
            known.label(),
            known.slot + 1
        ));
        let _ = events.send(Event::Log(format!(
            "Bonded {} restored in player slot {}; press a controller button to reconnect",
            known.label(),
            known.slot + 1
        )));
        let pending = spawn_ble_connection(
            known.adapter_index,
            known.device,
            known.slot,
            true,
            events.clone(),
            ble_tx.clone(),
            ble_scan_gate.clone(),
        );
        pending_ble.insert(addr, pending);
    }
    let mut ble_recovery_task: Option<(usize, JoinHandle<()>)> = None;
    if known_ble.is_empty() && ble_adapter_count > 0 {
        match default_adapter_address().await {
            Ok(host_address) => {
                diagnostic(&format!(
                    "No persisted Switch 2 controllers; recovering wake advertisements bonded to {host_address}"
                ));
                let _ = events.send(Event::Log(format!(
                    "Looking for existing Switch 2 bonds to {host_address}; press HOME"
                )));
                ble_recovery_task = Some((
                    0,
                    spawn_ble_bond_recovery(
                        0,
                        host_address,
                        events.clone(),
                        ble_recovery_tx.clone(),
                        ble_scan_gate.clone(),
                    ),
                ));
            }
            Err(error) => {
                let _ = events.send(Event::Log(format!(
                    "Could not recover existing Switch 2 bonds: {error}"
                )));
            }
        }
    }
    let mut br_scan_task: Option<JoinHandle<()>> = None;
    let mut ble_scan_task: Option<(usize, JoinHandle<()>)> = None;
    let mut controller_order_adapter = None;
    let mut identities = load_controller_identities(controller_store.as_ref());
    let cable_ble_host = default_adapter_address().await.ok().map(|address| {
        polymer_bluetooth::cable_pairing::CablePairingHost {
            address,
            adapter_index: Some(0),
        }
    });
    if let Some(host) = cable_ble_host {
        let _ = events.send(Event::CablePairing(crate::cable_pairing::Event::BleHost(
            host,
        )));
    }
    let mut cable_pairing = crate::cable_pairing::worker::CablePairingWorker::new(
        host.as_ref()
            .map(|host| polymer_bluetooth::cable_pairing::CablePairingHost {
                address: host.info().address,
                adapter_index: None,
            }),
        cable_ble_host,
        controller_store.clone(),
        cable_registry_error,
    );
    let mut listen_inbound = true;
    let _ = events.send(Event::Log(
        "Inbound reconnects enabled for known bonded controllers".into(),
    ));
    loop {
        tokio::select! {
            Some(result) = cable_pairing.receive() => {
                if let Ok(crate::cable_pairing::Outcome::Paired { device, state }) = &result {
                    if device.wireless_transport() == TransportKind::BrEdr {
                        identities.insert(state.controller_address, BrIdentity::new(Some(device.profile_id.clone())));
                    } else {
                        let addr = state.controller_address;
                        let slot = known_ble.get(&addr).map_or_else(
                            || next_slot(&active, &pending_inbound, &pending_outbound, &pending_ble),
                            |known| known.slot,
                        );
                        let known = KnownBle {
                            adapter_index: cable_ble_host.and_then(|host| host.adapter_index).unwrap_or(0),
                            profile_id: device.profile_id.clone(),
                            device: BleDevice {
                                addr, name: Some(device.name.clone()), rssi: None,
                                services: Vec::new(), manufacturer_data: HashMap::new(),
                            },
                            slot,
                        };
                        known_ble.insert(addr, known.clone());
                        if let Err(error) = save_known_ble_controllers(controller_store.as_ref(), &known_ble) {
                            let _ = events.send(Event::Log(format!("Could not save cable-paired player slot: {error}")));
                        }
                        if !pending_ble.contains_key(&addr) && !active.contains_key(&addr) {
                            pending_ble.insert(addr, spawn_ble_connection(
                                known.adapter_index, known.device, known.slot, true,
                                events.clone(), ble_tx.clone(), ble_scan_gate.clone(),
                            ));
                        }
                    }
                }
                let _ = events.send(Event::CablePairing(crate::cable_pairing::Event::Finished(
                    result.map_err(|error| error.to_string()),
                )));
            }
            Some(recovery) = ble_recovery_rx.recv() => {
                if let Some((_, task)) = ble_recovery_task.take() {
                    let _ = task.await;
                }
                let addr = recovery.device.addr;
                if known_ble.contains_key(&addr) || pending_ble.contains_key(&addr) {
                    continue;
                }
                let slot = next_slot(
                    &active,
                    &pending_inbound,
                    &pending_outbound,
                    &pending_ble,
                );
                let known = KnownBle {
                    adapter_index: recovery.adapter_index,
                    profile_id: controller_for_ble(&recovery.device)
                        .map(|controller| controller.id())
                        .unwrap_or_else(|| ProfileId::new("nintendo.switch2.unknown")),
                    device: recovery.device,
                    slot,
                };
                diagnostic(&format!(
                    "Recovered existing Switch 2 bond for {addr} ({}) in player slot {}",
                    known.label(),
                    slot + 1
                ));
                let _ = events.send(Event::Log(format!(
                    "Recovered bonded {} at {addr}; connecting…",
                    known.label()
                )));
                known_ble.insert(addr, known.clone());
                if let Err(error) = save_known_ble_controllers(controller_store.as_ref(), &known_ble) {
                    let _ = events.send(Event::Log(format!(
                        "Could not persist recovered Switch 2 controller: {error}"
                    )));
                }
                let pending = spawn_ble_connection(
                    known.adapter_index,
                    known.device,
                    known.slot,
                    true,
                    events.clone(),
                    ble_tx.clone(),
                    ble_scan_gate.clone(),
                );
                pending_ble.insert(addr, pending);
            }
            Some(completion) = ble_rx.recv() => {
                let is_current = pending_ble
                    .get(&completion.addr)
                    .is_some_and(|pending| pending.slot == completion.slot);
                if !is_current {
                    if let Ok(mut connection) = completion.result {
                        let _ = connection.transport.disconnect().await;
                    }
                    continue;
                }
                pending_ble.remove(&completion.addr);
                match completion.result {
                    Ok(BleConnection {
                        transport,
                        mut notifications,
                        mut disconnects,
                        session,
                    }) => {
                        known_ble.insert(
                            completion.addr,
                            KnownBle {
                                adapter_index: completion.adapter_index,
                                device: completion.device.clone(),
                                profile_id: completion.profile_id.clone(),
                                slot: completion.slot,
                            },
                        );
                        if let Err(error) = save_known_ble_controllers(controller_store.as_ref(), &known_ble) {
                            let _ = events.send(Event::Log(format!(
                                "Could not persist bonded Switch 2 controller: {error}"
                            )));
                        }
                        stop_active(&mut active, completion.addr).await;
                        let report_events = events.clone();
                        let disconnect_tx = ble_disconnect_tx.clone();
                        let addr = completion.addr;
                        let slot = completion.slot;
                        let mut report_decoder = decoder::for_profile(&completion.profile_id);
                        let report_task = tokio::spawn(async move {
                            loop {
                                tokio::select! {
                                    result = notifications.recv() => match result {
                                        Ok(notification) => {
                                            if let Some(input) = report_decoder
                                                .as_mut()
                                                .and_then(|decoder| decoder.decode(
                                                    &notification.value,
                                                    notification.received_at,
                                                ))
                                            {
                                                let _ = report_events.send(Event::DecodedInput {
                                                    addr: addr.to_string(),
                                                    input,
                                                });
                                            }
                                            let _ = report_events.send(Event::BleReport {
                                                addr: addr.to_string(),
                                                characteristic: notification.uuid.to_string(),
                                                bytes: notification.value.to_vec(),
                                                received_at: notification.received_at,
                                            });
                                        }
                                        Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                                            let _ = report_events.send(Event::BleReportsDropped {
                                                addr: addr.to_string(),
                                                count,
                                            });
                                        }
                                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                                    },
                                    result = disconnects.recv() => {
                                        if result.is_ok() {
                                            let _ = disconnect_tx.send(BleDisconnected { addr, slot });
                                        }
                                        break;
                                    }
                                }
                            }
                        });
                        active.insert(completion.addr, ActiveController {
                            transport: ActiveTransport::Ble(Box::new(transport)),
                            report_task,
                            identity: BrIdentity::new(Some(completion.profile_id.clone())),
                            slot: completion.slot,
                            session: Some(session),
                        });
                        diagnostic(&format!(
                            "Switch 2 GATT notifications and wake reconnect ready for {} in player slot {}",
                            completion.addr,
                            completion.slot + 1
                        ));
                        let _ = events.send(Event::Connected {
                            addr: completion.addr.to_string(),
                            name: ControllerRegistry::standard()
                                .get(&completion.profile_id)
                                .map_or("Switch 2 controller", |controller| controller.display_name())
                                .into(),
                            radio: RadioKind::Ble,
                            slot: completion.slot,
                        });
                    }
                    Err(error) => {
                        diagnostic(&format!(
                            "Switch 2 BLE connection failed for {}: {error}",
                            completion.addr
                        ));
                        let message = format!("{} BLE connection failed: {error}", completion.addr);
                        if completion.automatic {
                            let _ = events.send(Event::Log(message));
                        } else {
                            let _ = events.send(Event::Error(message));
                        }
                    }
                }
                if let Some(adapter_index) = controller_order_adapter {
                    start_controller_order_scans(
                        adapter_index,
                        (
                            pending_inbound.is_empty() && pending_outbound.is_empty(),
                            pending_ble.is_empty() && ble_adapter_count > 0,
                        ),
                        host.as_ref(),
                        &events,
                        &ble_scan_gate,
                        &mut br_scan_task,
                        &mut ble_scan_task,
                    );
                }
            }
            Some(disconnected) = ble_disconnect_rx.recv() => {
                let is_current = active
                    .get(&disconnected.addr)
                    .is_some_and(|controller| {
                        controller.slot == disconnected.slot
                            && matches!(&controller.transport, ActiveTransport::Ble(_))
                    });
                if !is_current {
                    continue;
                }
                stop_active(&mut active, disconnected.addr).await;
                let _ = events.send(Event::Disconnected {
                    addr: disconnected.addr.to_string(),
                    reason: "Switch 2 controller asleep; waiting for a wake advertisement…".into(),
                });
                let Some(known) = known_ble.get(&disconnected.addr).cloned() else {
                    continue;
                };
                if pending_ble.contains_key(&disconnected.addr) {
                    continue;
                }
                diagnostic(&format!(
                    "Waiting for wake advertisement from Switch 2 controller {}",
                    disconnected.addr
                ));
                let _ = events.send(Event::Log(format!(
                    "Wake reconnect armed for {}; press a controller button",
                    disconnected.addr
                )));
                let pending = spawn_ble_connection(
                    known.adapter_index,
                    known.device,
                    known.slot,
                    true,
                    events.clone(),
                    ble_tx.clone(),
                    ble_scan_gate.clone(),
                );
                pending_ble.insert(disconnected.addr, pending);
            }
            Some(completion) = outbound_rx.recv() => {
                let is_current = pending_outbound
                    .get(&completion.addr)
                    .is_some_and(|pending| pending.slot == completion.slot);
                if !is_current {
                    if let Ok(mut connection) = completion.result {
                        let _ = connection.transport.disconnect().await;
                    }
                    continue;
                }
                pending_outbound.remove(&completion.addr);
                match completion.result {
                    Ok(BrConnection {
                        transport,
                        reports,
                        session,
                    }) => {
                        listen_inbound = true;
                        if completion.identity.token().is_some() {
                            identities.insert(completion.addr, completion.identity.clone());
                            if let Err(error) = save_controller_identities(controller_store.as_ref(), &identities) {
                                let _ = events.send(Event::Log(format!(
                                    "Could not persist controller identity: {error}"
                                )));
                            }
                        }
                        stop_active(&mut active, completion.addr).await;
                        let report_task = spawn_bredr_report_task(
                            reports,
                            events.clone(),
                            completion.addr,
                            completion.identity.clone(),
                            session.as_ref().and_then(|session| session.ds3_motion()),
                            session.as_ref().and_then(|session| session.sensor_calibration()),
                        );
                        active.insert(
                            completion.addr,
                            ActiveController {
                                transport: ActiveTransport::BrEdr(transport),
                                report_task,
                                identity: completion.identity.clone(),
                                slot: completion.slot,
                                session,
                            },
                        );
                        diagnostic(&format!(
                            "BR/EDR HID channels connected for {} in player slot {}",
                            completion.addr,
                            completion.slot + 1
                        ));
                        let _ = events.send(Event::Connected {
                            addr: completion.addr.to_string(),
                            name: completion.identity.label().into(),
                            radio: RadioKind::BrEdr,
                            slot: completion.slot,
                        });
                    }
                    Err(error) => {
                        diagnostic(&format!(
                            "BR/EDR connection failed for {}: {error}",
                            completion.addr
                        ));
                        let _ = events.send(Event::Error(format!(
                            "{} connection failed: {error}",
                            completion.addr
                        )));
                    }
                }
                if let Some(adapter_index) = controller_order_adapter {
                    start_controller_order_scans(
                        adapter_index,
                        (
                            pending_inbound.is_empty() && pending_outbound.is_empty(),
                            pending_ble.is_empty() && ble_adapter_count > 0,
                        ),
                        host.as_ref(),
                        &events,
                        &ble_scan_gate,
                        &mut br_scan_task,
                        &mut ble_scan_task,
                    );
                }
            }
            Some(completion) = inbound_rx.recv() => {
                let is_current = pending_inbound
                    .get(&completion.addr)
                    .is_some_and(|pending| pending.handle == completion.handle);
                if !is_current {
                    if let Ok(mut connection) = completion.result {
                        let _ = connection.transport.disconnect().await;
                    }
                    continue;
                }
                pending_inbound.remove(&completion.addr);
                match completion.result {
                    Ok(BrConnection {
                        transport,
                        reports,
                        session,
                    }) => {
                        stop_active(&mut active, completion.addr).await;
                        let report_task = spawn_bredr_report_task(
                            reports,
                            events.clone(),
                            completion.addr,
                            completion.identity.clone(),
                            session.as_ref().and_then(|session| session.ds3_motion()),
                            session.as_ref().and_then(|session| session.sensor_calibration()),
                        );
                        active.insert(
                            completion.addr,
                            ActiveController {
                                transport: ActiveTransport::BrEdr(transport),
                                report_task,
                                identity: completion.identity.clone(),
                                slot: completion.slot,
                                session,
                            },
                        );
                        let _ = events.send(Event::Connected {
                            addr: completion.addr.to_string(),
                            name: completion.identity.label().into(),
                            radio: RadioKind::BrEdr,
                            slot: completion.slot,
                        });
                    }
                    Err(error) => {
                        if completion.identity.is_switch() {
                            let _ = events.send(Event::Log(
                                "Nintendo reconnect failed; accepting its next automatic page retry"
                                    .into(),
                            ));
                        }
                        let _ = events.send(Event::Error(format!(
                            "{} reconnect failed: {error}",
                            completion.addr
                        )));
                    }
                }
                if let Some(adapter_index) = controller_order_adapter {
                    start_controller_order_scans(
                        adapter_index,
                        (
                            pending_inbound.is_empty() && pending_outbound.is_empty(),
                            pending_ble.is_empty() && ble_adapter_count > 0,
                        ),
                        host.as_ref(),
                        &events,
                        &ble_scan_gate,
                        &mut br_scan_task,
                        &mut ble_scan_task,
                    );
                }
            }
            command = commands.recv() => {
                let Some(command) = command else { break; };
                match command {
                    Command::CablePairing(action) => cable_pairing.start(action),
                    Command::BeginControllerOrder { adapter_index } => {
                        controller_order_adapter = Some(adapter_index);
                        listen_inbound = true;
                        stop_br_scan(&mut br_scan_task, host.as_ref(), &events).await;
                        stop_ble_scan(&mut ble_scan_task, &events).await;
                        stop_ble_recovery(&mut ble_recovery_task).await;

                        for (_, pending) in pending_ble.drain() {
                            pending.task.abort();
                            let _ = pending.task.await;
                        }
                        if ble_adapter_count > 0 {
                            let _ = stop_ble_adapter_scan(adapter_index).await;
                        }
                        for (_, pending) in pending_outbound.drain() {
                            pending.task.abort();
                            let _ = pending.task.await;
                        }
                        for (_, pending) in pending_inbound.drain() {
                            pending.task.abort();
                            let _ = pending.task.await;
                            if let Some(host) = &host {
                                let _ = host.disconnect(pending.handle).await;
                            }
                        }
                        sleep_all_for_controller_order(&mut active, &events).await;

                        start_controller_order_scans(
                            adapter_index,
                            (true, ble_adapter_count > 0),
                            host.as_ref(),
                            &events,
                            &ble_scan_gate,
                            &mut br_scan_task,
                            &mut ble_scan_task,
                        );
                        let _ = events.send(Event::Idle(
                            "Controller order mode ready; wake or pair controllers in player order"
                                .into(),
                        ));
                    }
                    Command::EndControllerOrder => {
                        controller_order_adapter = None;
                        stop_br_scan(&mut br_scan_task, host.as_ref(), &events).await;
                        stop_ble_scan(&mut ble_scan_task, &events).await;
                        for known in known_ble.values().cloned().collect::<Vec<_>>() {
                            let addr = known.device.addr;
                            if active.contains_key(&addr) || pending_ble.contains_key(&addr) {
                                continue;
                            }
                            let pending = spawn_ble_connection(
                                known.adapter_index,
                                known.device,
                                known.slot,
                                true,
                                events.clone(),
                                ble_tx.clone(),
                                ble_scan_gate.clone(),
                            );
                            pending_ble.insert(addr, pending);
                        }
                        let _ = events.send(Event::Idle(
                            "Controller order saved; bonded wake reconnects remain armed".into(),
                        ));
                    }
                    Command::ConnectBr {
                        device,
                        pin,
                        mut profile_id,
                    } => {
                        let (Some(host), Some(pairing)) = (&host, &pairing) else {
                            let _ = events.send(Event::Error("dedicated BR/EDR adapter or pairing store is unavailable".into()));
                            continue;
                        };
                        if pending_outbound.contains_key(&device.addr) {
                            let _ = events.send(Event::Error(format!(
                                "a connection attempt for {} is already running",
                                device.addr
                            )));
                            continue;
                        }
                        stop_br_scan(&mut br_scan_task, Some(host), &events).await;
                        if controller_order_adapter.is_none() {
                            stop_ble_scan(&mut ble_scan_task, &events).await;
                        }
                        if let Some(pending) = pending_inbound.remove(&device.addr) {
                            pending.task.abort();
                            let _ = host.disconnect(pending.handle).await;
                        }
                        stop_active(&mut active, device.addr).await;
                        profile_id = identities.get(&device.addr)
                            .and_then(|identity| identity.profile_id.clone()).or(profile_id);
                        let mut identity = BrIdentity::new(profile_id);
                        if identity.is_switch() || identity.is_wii_remote() {
                            if let Err(error) = pairing.forget(device.addr) {
                                let _ = events.send(Event::Error(format!(
                                    "could not clear the previous Nintendo bond: {error}"
                                )));
                                continue;
                            }
                            diagnostic(&format!(
                                "Forcing fresh pairing for Nintendo controller {}",
                                device.addr
                            ));
                            let _ = events.send(Event::Log(
                                "Cleared the previous Nintendo bond for fresh pairing".into(),
                            ));
                        }
                        if let Err(error) = configure_pin(pairing, device.addr, pin, &identity) {
                            let _ = events.send(Event::Error(error.to_string()));
                            continue;
                        }
                        let pin_source = match pin {
                            BondingMode::Ssp if identity.is_switch() => {
                                "SSP Just Works; legacy fallback PIN = 0000".to_owned()
                            }
                            BondingMode::Ssp => "SSP / no legacy PIN".to_owned(),
                            BondingMode::WiiOneTwo => {
                                format!("temporary PIN = reverse of Wii address {}", device.addr)
                            }
                            BondingMode::WiiSync => format!(
                                "bonding PIN = reverse of host address {}",
                                host.info().address
                            ),
                        };
                        let _ = events.send(Event::Log(format!(
                            "Pairing policy for {}: {pin_source}",
                            device.addr
                        )));
                        diagnostic(&format!("Pairing policy for {}: {pin_source}", device.addr));
                        let _ = events.send(Event::Busy(format!("Connecting and authenticating {}…", device.addr)));
                        let mut transport = BrEdrHid::new(host.clone());
                        if identity.is_wii_remote() {
                            // Keep the controller's default retransmission policy until
                            // the four-controller latency value is tuned on Wii hardware.
                            transport.set_automatic_flush_slots(0);
                        }
                        let reports = transport.reports();
                        let slot = next_slot(
                            &active,
                            &pending_inbound,
                            &pending_outbound,
                            &pending_ble,
                        );
                        let addr = device.addr;
                        let completion_tx = outbound_tx.clone();
                        let task_events = events.clone();
                        let task_host = host.clone();
                        let task = tokio::spawn(async move {
                            let result = async {
                                // During fresh SSP pairing the host opens both HID
                                // PSMs. Bonded button-wake reconnects use the opposite,
                                // controller-initiated direction handled below.
                                identity.establish(&task_host, &device, &mut transport).await?;

                                let session = match identity.connect(&mut transport, slot).await {
                                    Ok(session) => session,
                                    Err(error) => {
                                        let _ = transport.disconnect().await;
                                        return Err(error);
                                    }
                                };
                                if identity.is_wii_remote() {
                                    diagnostic(&format!(
                                        "Initialized Wii Remote {addr}: player LED {}, continuous buttons+accelerometer+IR+MotionPlus reports",
                                        slot + 1
                                    ));
                                    let _ = task_events.send(Event::Log(format!(
                                        "Wii Remote player LED {}, IR camera, MotionPlus, and continuous input enabled",
                                        slot + 1
                                    )));
                                } else if identity.is_switch() {
                                    diagnostic(&format!(
                                        "Initialized {} {addr}: native 0x30 reports, vibration, player LED {}",
                                        identity.label(),
                                        slot + 1
                                    ));
                                    let _ = task_events.send(Event::Log(format!(
                                        "{} native input enabled in player slot {}",
                                        identity.label(),
                                        slot + 1
                                    )));
                                }
                                Ok(BrConnection {
                                    transport,
                                    reports,
                                    session,
                                })
                            }
                            .await;
                            let _ = completion_tx.send(OutboundResult {
                                addr,
                                identity,
                                slot,
                                result,
                            });
                        });
                        pending_outbound.insert(addr, PendingOutbound { slot, task });
                    }
                    Command::ConnectBle { adapter_index, device } => {
                        let Some(protocol) = controller_for_ble(&device).filter(|controller| {
                            controller.id().as_str().starts_with("nintendo.switch2.")
                        }) else {
                            let _ = events.send(Event::Error(format!(
                                "{} is not a supported Switch 2 controller",
                                device.addr
                            )));
                            continue;
                        };
                        if pending_ble.contains_key(&device.addr) {
                            let _ = events.send(Event::Error(format!(
                                "a BLE connection attempt for {} is already running",
                                device.addr
                            )));
                            continue;
                        }
                        stop_ble_recovery(&mut ble_recovery_task).await;
                        if controller_order_adapter.is_none() {
                            stop_br_scan(&mut br_scan_task, host.as_ref(), &events).await;
                        }
                        stop_ble_scan(&mut ble_scan_task, &events).await;
                        let addr = device.addr;
                        stop_active(&mut active, addr).await;
                        let slot = next_slot(
                            &active,
                            &pending_inbound,
                            &pending_outbound,
                            &pending_ble,
                        );
                        let pairing_mode = !protocol.accepts_reconnect_advertisement(
                            &ble_device_identity(&device),
                            None,
                        );
                        let automatic = controller_order_adapter.is_none()
                            && known_ble.contains_key(&addr)
                            && !pairing_mode;
                        let _ = events.send(Event::Busy(format!("Connecting to {} over BLE…", protocol.display_name())));
                        diagnostic(&format!(
                            "Starting Switch 2 BLE connection to {addr} ({}) on OS adapter {adapter_index}",
                            protocol.display_name()
                        ));
                        let pending = spawn_ble_connection(
                            adapter_index,
                            device,
                            slot,
                            automatic,
                            events.clone(),
                            ble_tx.clone(),
                            ble_scan_gate.clone(),
                        );
                        pending_ble.insert(addr, pending);
                    }
                    Command::SelectSwitch2InputReport { addr, report_id } => {
                        let Some(known) = known_ble.get(&addr) else {
                            let _ = events.send(Event::Error(format!(
                                "no known Switch 2 controller at {addr}"
                            )));
                            continue;
                        };
                        let Some(protocol) = known.controller() else {
                            let _ = events.send(Event::Error(format!(
                                "controller profile {} is unavailable",
                                known.profile_id
                            )));
                            continue;
                        };
                        let Some(controller) = active.get_mut(&addr) else {
                            let _ = events.send(Event::Error(format!(
                                "no connected Switch 2 controller at {addr}"
                            )));
                            continue;
                        };
                        let ActiveTransport::Ble(transport) = &mut controller.transport else {
                            let _ = events.send(Event::Error(format!(
                                "controller {addr} is not connected over BLE"
                            )));
                            continue;
                        };
                        let identity = ble_device_identity(&known.device);
                        let Some(profile) = protocol.gatt_profile(&identity).ok().flatten() else {
                            let _ = events.send(Event::Error(format!(
                                "{} does not expose a GATT input profile",
                                protocol.display_name()
                            )));
                            continue;
                        };
                        let input_uuid = if report_id == 0x05 {
                            COMMON_INPUT_UUID
                        } else if Some(report_id) == protocol.preferred_input_report_id() {
                            profile.input
                        } else {
                            let _ = events.send(Event::Error(format!(
                                "input report 0x{report_id:02X} does not belong to {}",
                                protocol.display_name()
                            )));
                            continue;
                        };
                        match transport.select_input_characteristic(input_uuid).await {
                            Ok(()) => {
                                let _ = events.send(Event::Switch2InputReportSelected {
                                    addr: addr.to_string(),
                                    report_id,
                                });
                                let _ = events.send(Event::Log(format!(
                                    "{addr}: selected Switch 2 input report 0x{report_id:02X}"
                                )));
                            }
                            Err(error) => {
                                let _ = events.send(Event::Switch2InputReportSelectionFailed {
                                    addr: addr.to_string(),
                                    report_id,
                                    reason: error.to_string(),
                                });
                                let _ = events.send(Event::Error(format!(
                                    "could not select input report 0x{report_id:02X} for {addr}: {error}"
                                )));
                            }
                        }
                    }
                    Command::SubscribeAllSwitch2InputReports { addr } => {
                        let Some(known) = known_ble.get(&addr) else {
                            let _ = events.send(Event::Switch2InputReportsSubscriptionFailed {
                                addr: addr.to_string(),
                                reason: "controller is not known".into(),
                            });
                            continue;
                        };
                        let Some(protocol) = known.controller() else {
                            let _ = events.send(Event::Switch2InputReportsSubscriptionFailed {
                                addr: addr.to_string(),
                                reason: format!("controller profile {} is unavailable", known.profile_id),
                            });
                            continue;
                        };
                        let Some(native_report_id) = protocol.preferred_input_report_id() else {
                            let _ = events.send(Event::Switch2InputReportsSubscriptionFailed {
                                addr: addr.to_string(),
                                reason: format!("{} has no native input report", protocol.display_name()),
                            });
                            continue;
                        };
                        let Some(controller) = active.get_mut(&addr) else {
                            let _ = events.send(Event::Switch2InputReportsSubscriptionFailed {
                                addr: addr.to_string(),
                                reason: "controller is not connected".into(),
                            });
                            continue;
                        };
                        let ActiveTransport::Ble(transport) = &mut controller.transport else {
                            let _ = events.send(Event::Switch2InputReportsSubscriptionFailed {
                                addr: addr.to_string(),
                                reason: "controller is not connected over BLE".into(),
                            });
                            continue;
                        };
                        match transport.subscribe_all_input_characteristics().await {
                            Ok(uuids) => {
                                let report_ids = uuids
                                    .into_iter()
                                    .filter_map(|uuid| {
                                        (uuid == COMMON_INPUT_UUID)
                                            .then_some(0x05)
                                            .or_else(|| (uuid != COMMON_INPUT_UUID).then_some(native_report_id))
                                    })
                                    .collect::<Vec<_>>();
                                let _ = events.send(Event::Switch2InputReportsSubscribed {
                                    addr: addr.to_string(),
                                    report_ids: report_ids.clone(),
                                });
                                let reports = report_ids
                                    .iter()
                                    .map(|report_id| format!("0x{report_id:02X}"))
                                    .collect::<Vec<_>>()
                                    .join(" and ");
                                let _ = events.send(Event::Log(format!(
                                    "{addr}: subscribed Switch 2 input reports {reports}"
                                )));
                            }
                            Err(error) => {
                                let _ = events.send(Event::Switch2InputReportsSubscriptionFailed {
                                    addr: addr.to_string(),
                                    reason: error.to_string(),
                                });
                            }
                        }
                    }
                    Command::ListenInbound(enabled) => {
                        listen_inbound = enabled;
                        let _ = events.send(Event::Log(if enabled { "Inbound ACL acceptance enabled".into() } else { "Inbound ACL acceptance disabled".into() }));
                    }
                    Command::Forget(addr) => {
                        if let Some(pending) = pending_inbound.remove(&addr) {
                            pending.task.abort();
                            if let Some(host) = &host {
                                let _ = host.disconnect(pending.handle).await;
                            }
                        }
                        stop_active(&mut active, addr).await;
                        let Some(pairing) = &pairing else {
                            let _ = events.send(Event::Error("BR/EDR pairing store is unavailable".into()));
                            continue;
                        };
                        match pairing.forget(addr) {
                            Ok(()) => {
                                identities.remove(&addr);
                                if let Err(error) = save_controller_identities(controller_store.as_ref(), &identities) {
                                    let _ = events.send(Event::Log(format!("Could not update controller identities: {error}")));
                                }
                                let _ = events.send(Event::Idle(format!("Forgot link key and controller identity for {addr}")));
                            }
                            Err(error) => { let _ = events.send(Event::Error(error.to_string())); }
                        }
                    }
                    Command::SendOutput { addr, bytes } => {
                        match active.get_mut(&addr) {
                            Some(active) => match active.transport.send(&bytes).await {
                                Ok(()) => { let _ = events.send(Event::Log(format!("Sent {} output bytes", bytes.len()))); }
                                Err(error) => { let _ = events.send(Event::Error(error.to_string())); }
                            },
                            None => { let _ = events.send(Event::Error(format!("no connected HID controller at {addr}"))); }
                        }
                    }
                    Command::SleepBle(addr) => {
                        let Some(known) = known_ble.get(&addr).cloned() else {
                            let _ = events.send(Event::Error(format!(
                                "no bonded Switch 2 wake metadata for {addr}"
                            )));
                            continue;
                        };
                        let Some(mut controller) = active.remove(&addr) else {
                            let _ = events.send(Event::Error(format!(
                                "no connected Switch 2 controller at {addr}"
                            )));
                            continue;
                        };
                        let sleep_result = if matches!(&controller.transport, ActiveTransport::Ble(_)) {
                            controller.sleep().await
                        } else {
                            Err(polymer_bluetooth::Error::Unsupported(
                                "Switch 2 sleep is only available for BLE sessions",
                            ))
                        };
                        if let Err(error) = sleep_result {
                            active.insert(addr, controller);
                            let _ = events.send(Event::Error(format!(
                                "could not put Switch 2 controller {addr} to sleep: {error}"
                            )));
                            continue;
                        }

                        diagnostic(&format!(
                            "Sent bonded sleep command to Switch 2 controller {addr}"
                        ));
                        controller.report_task.abort();
                        // Let WinRT submit the write before closing our local
                        // GATT session. The command itself has no response.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        let _ = controller.transport.disconnect().await;
                        let _ = events.send(Event::Disconnected {
                            addr: addr.to_string(),
                            reason: "Switch 2 controller sleeping; wake reconnect is armed".into(),
                        });
                        let _ = events.send(Event::Log(format!(
                            "Wake reconnect armed for {addr}; press HOME or another controller button"
                        )));
                        let pending = spawn_ble_connection(
                            known.adapter_index,
                            known.device,
                            known.slot,
                            true,
                            events.clone(),
                            ble_tx.clone(),
                            ble_scan_gate.clone(),
                        );
                        pending_ble.insert(addr, pending);
                    }
                    Command::Disconnect(addr) => {
                        let targets = match addr {
                            Some(addr) => vec![addr],
                            None => active.keys().copied().collect(),
                        };
                        for addr in targets {
                            disconnect_active(&mut active, addr, &events).await;
                        }
                    }
                    Command::Shutdown => break,
                }
            }
            event = receive_hci(&mut hci_events) => {
                let Some(event) = event else { continue; };
                let event = match event {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => { let _ = events.send(Event::Log(format!("HCI monitor lagged by {count} events"))); continue; }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                };
                match event {
                    HciEvent::ConnectionRequest { addr, .. } if listen_inbound => {
                        let Some(host) = &host else { continue; };
                        let Some(identity) = identities.get(&addr).cloned() else {
                            diagnostic(&format!("Rejecting inbound connection from unknown address {addr}"));
                            let _ = host.reject_connection(addr).await;
                            continue;
                        };
                        stop_br_scan(&mut br_scan_task, Some(host), &events).await;
                        if pending_inbound.contains_key(&addr)
                            || pending_outbound.contains_key(&addr)
                        {
                            diagnostic(&format!(
                                "Rejecting duplicate inbound connection while {addr} is already attaching"
                            ));
                            let _ = host.reject_connection(addr).await;
                            continue;
                        }
                        stop_active(&mut active, addr).await;

                        let _ = events.send(Event::Busy(format!("Accepting inbound connection from {addr}…")));
                        // Subscribe before accepting the ACL. Nintendo sends an
                        // L2CAP Information Request immediately after Connection
                        // Complete, before a newly constructed HID transport can
                        // otherwise subscribe to the frame broadcast.
                        let early_frames = host.l2cap_frames();
                        let early_hci_events = host.events();
                        let become_master = !identity.is_switch();
                        match host.accept_connection_with_role(addr, become_master).await {
                            Ok(handle) => {
                                let mut transport = BrEdrHid::new(host.clone());
                                if identity.is_wii_remote() {
                                    transport.set_automatic_flush_slots(0);
                                }
                                let reports = transport.reports();
                                let slot = next_slot(
                                    &active,
                                    &pending_inbound,
                                    &pending_outbound,
                                    &pending_ble,
                                );
                                let completion_tx = inbound_tx.clone();
                                let task_events = events.clone();
                                let task_host = host.clone();
                                let task = tokio::spawn(async move {
                                    let result = async {
                                        let policy = identity.policy(addr, slot);
                                        if policy.host_opens_hid_on_reconnect {
                                            transport.attach_inbound_host_hid_with_frames(handle, early_frames).await?;
                                        } else if policy.wait_for_encryption_before_inbound_hid {
                                            transport
                                                .attach_inbound_encrypted_with_frames(
                                                    handle,
                                                    early_frames,
                                                    early_hci_events,
                                                )
                                                .await?;
                                        } else {
                                            transport
                                                .attach_inbound_with_frames(handle, early_frames)
                                                .await?;
                                        }

                                        let session = identity.connect(&mut transport, slot).await?;
                                        if identity.is_wii_remote() {
                                            let _ = task_events.send(Event::Log(format!(
                                                "Reconnected Wii Remote in player slot {}",
                                                slot + 1
                                            )));
                                        } else if identity.is_switch() {
                                            diagnostic(&format!(
                                                "Reconnected {} {addr}: native 0x30 reports and player LED {}",
                                                identity.label(),
                                                slot + 1
                                            ));
                                            let _ = task_events.send(Event::Log(format!(
                                                "Reconnected {} in player slot {} with native input",
                                                identity.label(),
                                                slot + 1
                                            )));
                                        } else if identity.token() == Some("sony.dualshock3") {
                                            let _ = task_events.send(Event::Log(format!(
                                                "Reconnected DualShock 3 in player slot {}; Bluetooth input activated",
                                                slot + 1
                                            )));
                                        }
                                        Ok(BrConnection {
                                            transport,
                                            reports,
                                            session,
                                        })
                                    }
                                    .await;
                                    if result.is_err() {
                                        let _ = task_host.disconnect(handle).await;
                                    }
                                    let _ = completion_tx.send(InboundResult {
                                        addr,
                                        handle,
                                        identity,
                                        slot,
                                        result,
                                    });
                                });
                                pending_inbound.insert(addr, PendingInbound { handle, slot, task });
                            }
                            Err(error) => { let _ = events.send(Event::Error(error.to_string())); }
                        }
                    }
                    HciEvent::PinCodeRequest { addr } => {
                        diagnostic(&format!("PIN requested by {addr}"));
                        let _ = events.send(Event::Log(format!("PIN requested by {addr}")));
                    }
                    HciEvent::LinkKeyRequest { addr } => {
                        diagnostic(&format!("Link key requested by {addr}"));
                        let _ = events.send(Event::Log(format!("Link key requested by {addr}")));
                    }
                    HciEvent::IoCapabilityRequest { addr } => { let _ = events.send(Event::Log(format!("SSP capability requested by {addr}"))); }
                    HciEvent::LinkKeyNotification { addr, .. } => {
                        diagnostic(&format!("Stored new link key for {addr}"));
                        let _ = events.send(Event::Log(format!("Stored new link key for {addr}")));
                    }
                    HciEvent::AuthenticationComplete { status, handle } => {
                        diagnostic(&format!("Authentication handle {handle}: status 0x{status:02X}"));
                        let _ = events.send(Event::Log(format!("Authentication handle {handle}: status 0x{status:02X}")));
                    }
                    HciEvent::EncryptionChange { status, handle, enabled } => {
                        diagnostic(&format!("Encryption handle {handle}: {enabled}, status 0x{status:02X}"));
                        let _ = events.send(Event::Log(format!("Encryption handle {handle}: {enabled}, status 0x{status:02X}")));
                    }
                    HciEvent::EncryptionKeyRefreshComplete { status, handle } => {
                        diagnostic(&format!("Encryption key refresh handle {handle}: status 0x{status:02X}"));
                        let _ = events.send(Event::Log(format!("Encryption key refresh handle {handle}: status 0x{status:02X}")));
                    }
                    HciEvent::DisconnectionComplete { status, handle, reason } => {
                        let active_addr = active.iter().find_map(|(addr, controller)| {
                            (controller.transport.bredr_handle() == Some(handle)).then_some(*addr)
                        });
                        let pending_addr = pending_inbound.iter().find_map(|(addr, pending)| {
                            (pending.handle == handle).then_some(*addr)
                        });
                        diagnostic(&format!(
                            "HCI disconnection complete: status 0x{status:02X}, handle {handle}, reason 0x{reason:02X}, active address {active_addr:?}, pending address {pending_addr:?}"
                        ));
                        if let Some(addr) = pending_addr
                            && let Some(pending) = pending_inbound.remove(&addr)
                        {
                            pending.task.abort();
                            let _ = events.send(Event::Log(format!(
                                "{addr} disconnected during HID setup (reason 0x{reason:02X}); ready for its next page retry"
                            )));
                        }
                        if let Some(addr) = active_addr
                            && let Some(controller) = active.remove(&addr)
                        {
                            controller.report_task.abort();
                            let _ = events.send(Event::Disconnected {
                                addr: addr.to_string(),
                                reason: format!("Controller disconnected (reason 0x{reason:02X})"),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    stop_br_scan(&mut br_scan_task, host.as_ref(), &events).await;
    stop_ble_scan(&mut ble_scan_task, &events).await;
    stop_ble_recovery(&mut ble_recovery_task).await;
    for (_, pending) in pending_inbound.drain() {
        pending.task.abort();
        if let Some(host) = &host {
            let _ = host.disconnect(pending.handle).await;
        }
    }
    for (_, pending) in pending_outbound.drain() {
        pending.task.abort();
    }
    for (_, pending) in pending_ble.drain() {
        pending.task.abort();
    }
    stop_all_active(&mut active).await;
    if let Some(task) = pairing_task {
        task.abort();
    }
}

async fn receive_hci(
    receiver: &mut Option<tokio::sync::broadcast::Receiver<HciEvent>>,
) -> Option<std::result::Result<HciEvent, tokio::sync::broadcast::error::RecvError>> {
    match receiver {
        Some(receiver) => Some(receiver.recv().await),
        None => std::future::pending().await,
    }
}

fn spawn_bredr_report_task(
    mut reports: ReportReceiver,
    events: mpsc::Sender<Event>,
    addr: DeviceAddr,
    identity: BrIdentity,
    motion: Option<polymer_bluetooth::devices::sony::dualshock3::motion::MotionState>,
    sensor_calibration: Option<polymer_bluetooth::devices::SensorCalibration>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut wiimote_decoder = wiimote::WiimoteTelemetryDecoder;
        let mut report_decoder = identity.profile_id.as_ref().and_then(decoder::for_profile);
        if let Some(decoder) = &mut report_decoder {
            decoder.set_ds3_motion(motion.clone());
            decoder.set_sensor_calibration(sensor_calibration);
        }
        let diagnostic_deadline = Instant::now() + Duration::from_secs(60);
        let mut next_diagnostic = Instant::now();
        loop {
            match reports.recv().await {
                Ok(report) => {
                    let received_at = Instant::now();
                    if identity.token() == Some("sony.dualshock3")
                        && received_at >= next_diagnostic
                        && received_at < diagnostic_deadline
                    {
                        diagnostic(&format!(
                            "DS3 native input {addr}: {:02X?}",
                            report.as_ref()
                        ));
                        diagnostic(&format!(
                            "DS3 motion calibration: {:?}",
                            motion.as_ref().and_then(|state| state.snapshot())
                        ));
                        next_diagnostic = received_at + Duration::from_millis(250);
                    }
                    if identity.is_wii_remote()
                        && let Some(telemetry) = wiimote_decoder.process(&report)
                    {
                        let _ = events.send(Event::WiiTelemetry {
                            addr: addr.to_string(),
                            telemetry,
                            received_at,
                        });
                    }
                    if let Some(input) = report_decoder
                        .as_mut()
                        .and_then(|decoder| decoder.decode(&report, received_at))
                    {
                        let _ = events.send(Event::DecodedInput {
                            addr: addr.to_string(),
                            input,
                        });
                    }
                    let _ = events.send(Event::Report {
                        addr: addr.to_string(),
                        bytes: report.to_vec(),
                    });
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                    let _ = events.send(Event::Log(format!("Dropped {count} raw reports")));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

fn spawn_ble_connection(
    adapter_index: usize,
    device: BleDevice,
    slot: usize,
    automatic: bool,
    events: mpsc::Sender<Event>,
    completion_tx: tokio_mpsc::UnboundedSender<BleResult>,
    scan_gate: Arc<Mutex<()>>,
) -> PendingBle {
    let addr = device.addr;
    let identity = ble_device_identity(&device);
    let protocol = controller_for_ble(&device)
        .filter(|controller| controller.id().as_str().starts_with("nintendo.switch2."))
        .unwrap_or_else(|| Arc::new(UnknownSwitch2Controller::default()));
    let profile_id = protocol.id();
    let pairing_mode = !automatic && !protocol.accepts_reconnect_advertisement(&identity, None);
    let result_device = device.clone();
    let task = tokio::spawn(async move {
        let profile = protocol.gatt_profile(&identity).and_then(|profile| {
            profile.ok_or(polymer_bluetooth::Error::Unsupported(
                "controller does not expose a GATT profile",
            ))
        });
        let mut session = protocol.connect(&connect_options(slot));
        let result = async {
        let mut transport = BleGatt::new(adapter_index, profile?);
        if automatic {
            // Rotate the single adapter fairly between dormant controllers.
            // A Switch 2 wake advertisement persists across several windows.
            transport.set_scan_timeout(Duration::from_secs(3));
        }
        let notifications = transport.gatt_notifications();
        let disconnects = transport.disconnections();
        loop {
            let connection = {
                let _scan_guard = scan_gate.lock().await;
                transport.connect(addr).await
            };
            match connection {
                Ok(()) => {
                    if pairing_mode {
                        let bond_result = async {
                            let host_address = default_adapter_address().await?;
                            protocol
                                .bond(&mut transport, &identity, host_address)
                                .await?;
                            Ok::<_, polymer_bluetooth::Error>(host_address)
                        }
                        .await;
                        match bond_result {
                            Ok(host_address) => {
                                diagnostic(&format!(
                                    "Committed Switch 2 bond for {addr} to OS adapter {}",
                                    host_address
                                ));
                                let _ = events.send(Event::Log(format!(
                                    "Bonded {addr} to {}; normal-button wake is enabled",
                                    host_address
                                )));
                            }
                            Err(error) => {
                                let _ = transport.disconnect().await;
                                break Err(error);
                            }
                        }
                    }
                    if let Err(error) = session.synchronize(&mut transport).await {
                        let _ = transport.disconnect().await;
                        break Err(error);
                    }
                    if let Err(error) = session.set_pairing_indicator(&mut transport, false).await {
                        let _ = transport.disconnect().await;
                        break Err(error);
                    }
                    diagnostic(&format!(
                        "{} {addr} acknowledged player LED {} with pairing indicator disabled",
                        protocol.display_name(),
                        slot + 1,
                    ));
                    break Ok(BleConnection {
                        transport,
                        notifications,
                        disconnects,
                        session,
                    });
                }
                Err(error) if !automatic => break Err(error),
                Err(polymer_bluetooth::Error::Unsupported(reason)) => {
                    break Err(polymer_bluetooth::Error::Unsupported(reason));
                }
                Err(polymer_bluetooth::Error::Backoff(delay)) => {
                    tokio::time::sleep(delay.max(Duration::from_millis(100))).await;
                }
                Err(error) if error.is_ble_device_absent() => {
                    // Sleeping is the normal steady state. The next scan waits
                    // for a fresh button-wake advertisement without consuming
                    // a controller cooldown attempt.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(error) => {
                    diagnostic(&format!(
                        "Wake reconnect attempt for {addr} failed: {error}; respecting BLE backoff"
                    ));
                    let _ = events.send(Event::Log(format!(
                        "Wake reconnect for {addr} will retry: {error}"
                    )));
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        }
        }.await;
        let _ = completion_tx.send(BleResult {
            addr,
            slot,
            adapter_index,
            device: result_device,
            profile_id,
            automatic,
            result,
        });
    });
    PendingBle { slot, task }
}

fn spawn_ble_bond_recovery(
    adapter_index: usize,
    host_address: DeviceAddr,
    events: mpsc::Sender<Event>,
    recovery_tx: tokio_mpsc::UnboundedSender<BleRecovery>,
    scan_gate: Arc<Mutex<()>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let result = {
                let _scan_guard = scan_gate.lock().await;
                scan_ble(adapter_index, Duration::from_secs(4)).await
            };
            match result {
                Ok(found) => {
                    if let Some(device) = found.into_iter().find(|device| {
                        controller_for_ble(device).is_some_and(|controller| {
                            controller.id().as_str().starts_with("nintendo.switch2.")
                                && controller.accepts_reconnect_advertisement(
                                    &ble_device_identity(device),
                                    Some(host_address),
                                )
                        })
                    }) {
                        let _ = recovery_tx.send(BleRecovery {
                            adapter_index,
                            device,
                        });
                        break;
                    }
                }
                Err(error) => {
                    diagnostic(&format!(
                        "Existing Switch 2 bond recovery scan failed: {error}"
                    ));
                    let _ = events.send(Event::Log(format!(
                        "Existing Switch 2 bond recovery will retry: {error}"
                    )));
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    })
}

fn spawn_br_scan(host: Arc<HciHost>, events: mpsc::Sender<Event>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut names: HashMap<DeviceAddr, String> = HashMap::new();
        let mut use_wii_liac = false;
        loop {
            let mode = if use_wii_liac {
                "Wii LIAC"
            } else {
                "general GIAC"
            };
            diagnostic(&format!("Starting BR/EDR inquiry cycle ({mode})"));
            let inquiry = if use_wii_liac {
                host.limited_inquiry(3, 0).await
            } else {
                host.inquiry(3, 0).await
            };
            use_wii_liac = !use_wii_liac;
            match inquiry {
                Ok(found) => {
                    diagnostic(&format!(
                        "BR/EDR inquiry cycle found {} device(s)",
                        found.len()
                    ));
                    // Publish the entire inquiry batch before any remote-name
                    // request. A slow device or cancelled scan must not hide it.
                    for inquiry in &found {
                        if !names.contains_key(&inquiry.addr) {
                            let mut view = device_view(inquiry.clone(), "Resolving device name…".into());
                            // Wait for identification before automatic pairing:
                            // a generic class match cannot choose Nintendo's PIN.
                            view.profile_id = None;
                            view.profile_name = None;
                            diagnostic(&format!("Discovered BR/EDR address {} (name pending)", inquiry.addr));
                            let _ = events.send(Event::Device(view));
                        }
                    }
                    for inquiry in found {
                        let name = if let Some(name) = names.get(&inquiry.addr) {
                            name.clone()
                        } else {
                            match host.remote_name(&inquiry).await {
                                Ok(name) if !name.trim().is_empty() => {
                                    diagnostic(&format!("Discovered {} as {name}", inquiry.addr));
                                    names.insert(inquiry.addr, name.clone());
                                    name
                                }
                                Ok(_) => {
                                    diagnostic(&format!(
                                        "Remote-name request for {} returned an empty name; will retry",
                                        inquiry.addr
                                    ));
                                    "Unknown controller".into()
                                }
                                Err(error) => {
                                    diagnostic(&format!(
                                        "Remote-name request for {} failed: {error}; will retry",
                                        inquiry.addr
                                    ));
                                    "Unknown controller".into()
                                }
                            }
                        };
                        let _ = events.send(Event::Device(device_view(inquiry, name)));
                    }
                }
                Err(error) => {
                    diagnostic(&format!("BR/EDR inquiry cycle failed: {error}"));
                    let _ = events.send(Event::Log(format!("BR/EDR inquiry will retry: {error}")));
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    })
}

fn device_view(inquiry: InquiryResult, name: String) -> DeviceView {
    let controller = controller_for_bredr(&inquiry, &name);
    DeviceView {
        addr: inquiry.addr.to_string(),
        name,
        rssi: inquiry.rssi,
        class: format!(
            "{:02X}{:02X}{:02X}",
            inquiry.class_of_device[2], inquiry.class_of_device[1], inquiry.class_of_device[0]
        ),
        profile_id: controller.as_ref().map(|controller| controller.id()),
        profile_name: controller.map(|controller| controller.display_name()),
        inquiry,
    }
}

fn controller_for_bredr(inquiry: &InquiryResult, name: &str) -> Option<Arc<dyn ControllerDevice>> {
    let class = inquiry.class_of_device;
    ControllerRegistry::standard()
        .identify(&DeviceIdentity {
            address: inquiry.addr,
            transport: TransportKind::BrEdr,
            name: Some(name.to_owned()),
            class_of_device: Some(
                u32::from(class[0]) | (u32::from(class[1]) << 8) | (u32::from(class[2]) << 16),
            ),
            service_uuids: Vec::new(),
            manufacturer_data: Vec::new(),
            pnp: None,
        })
        .ok()
        .flatten()
}

fn spawn_ble_scan(
    adapter_index: usize,
    events: mpsc::Sender<Event>,
    scan_gate: Arc<Mutex<()>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let result = {
                let _scan_guard = scan_gate.lock().await;
                scan_ble(adapter_index, Duration::from_secs(4)).await
            };
            match result {
                Ok(found) => {
                    for device in found.into_iter().filter(|device| {
                        controller_for_ble(device).is_some_and(|controller| {
                            controller.id().as_str().starts_with("nintendo.switch2.")
                        })
                    }) {
                        let _ = events.send(Event::BleDevice(device));
                    }
                }
                Err(error) => {
                    diagnostic(&format!("BLE scan cycle failed: {error}"));
                    let _ = events.send(Event::Log(format!("BLE scan will retry: {error}")));
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    })
}

async fn stop_br_scan(
    task: &mut Option<JoinHandle<()>>,
    host: Option<&Arc<HciHost>>,
    events: &mpsc::Sender<Event>,
) {
    let Some(task) = task.take() else {
        return;
    };
    task.abort();
    let _ = task.await;
    if let Some(host) = host {
        let _ = host.cancel_inquiry().await;
    }
    diagnostic("Continuous BR/EDR inquiry disabled");
    let _ = events.send(Event::Scanning {
        radio: RadioKind::BrEdr,
        enabled: false,
        message: "BR/EDR scanning stopped".into(),
    });
}

async fn stop_ble_scan(task: &mut Option<(usize, JoinHandle<()>)>, events: &mpsc::Sender<Event>) {
    let Some((adapter_index, task)) = task.take() else {
        return;
    };
    task.abort();
    let _ = task.await;
    if let Err(error) = stop_ble_adapter_scan(adapter_index).await {
        diagnostic(&format!("Could not stop OS BLE scan cleanly: {error}"));
    }
    diagnostic("Continuous BLE scan disabled");
    let _ = events.send(Event::Scanning {
        radio: RadioKind::Ble,
        enabled: false,
        message: "BLE scanning stopped".into(),
    });
}

async fn stop_ble_recovery(task: &mut Option<(usize, JoinHandle<()>)>) {
    let Some((adapter_index, task)) = task.take() else {
        return;
    };
    task.abort();
    let _ = task.await;
    let _ = stop_ble_adapter_scan(adapter_index).await;
}

fn configure_pin(
    pairing: &PairingAgent,
    addr: DeviceAddr,
    choice: BondingMode,
    identity: &BrIdentity,
) -> polymer_bluetooth::Result<()> {
    match choice {
        BondingMode::Ssp if identity.is_switch() => {
            pairing.set_pin(addr, PinCode::Fixed(b"0000".to_vec()))
        }
        BondingMode::Ssp => pairing.clear_pin(addr),
        BondingMode::WiiOneTwo => pairing.set_pin(addr, PinCode::RemoteAddress),
        BondingMode::WiiSync => pairing.set_pin(addr, PinCode::LocalAddress),
    }
}

fn next_slot(
    active: &HashMap<DeviceAddr, ActiveController>,
    pending: &HashMap<DeviceAddr, PendingInbound>,
    outbound: &HashMap<DeviceAddr, PendingOutbound>,
    ble: &HashMap<DeviceAddr, PendingBle>,
) -> usize {
    (0..4)
        .find(|slot| {
            !active.values().any(|controller| controller.slot == *slot)
                && !pending.values().any(|controller| controller.slot == *slot)
                && !outbound.values().any(|controller| controller.slot == *slot)
                && !ble.values().any(|controller| controller.slot == *slot)
        })
        .unwrap_or_else(|| (active.len() + pending.len() + outbound.len() + ble.len()) % 4)
}

fn start_controller_order_scans(
    adapter_index: usize,
    scan_radios: (bool, bool),
    host: Option<&Arc<HciHost>>,
    events: &mpsc::Sender<Event>,
    ble_scan_gate: &Arc<Mutex<()>>,
    br_scan_task: &mut Option<JoinHandle<()>>,
    ble_scan_task: &mut Option<(usize, JoinHandle<()>)>,
) {
    let (scan_br, scan_ble) = scan_radios;
    if scan_br
        && br_scan_task.is_none()
        && let Some(host) = host
    {
        diagnostic("Controller order: BR/EDR inquiry enabled");
        *br_scan_task = Some(spawn_br_scan(host.clone(), events.clone()));
        let _ = events.send(Event::Scanning {
            radio: RadioKind::BrEdr,
            enabled: true,
            message: "Controller order: BR/EDR discovery active".into(),
        });
    }
    if scan_ble && ble_scan_task.is_none() {
        diagnostic(&format!(
            "Controller order: BLE discovery enabled on OS adapter {adapter_index}"
        ));
        *ble_scan_task = Some((
            adapter_index,
            spawn_ble_scan(adapter_index, events.clone(), ble_scan_gate.clone()),
        ));
        let _ = events.send(Event::Scanning {
            radio: RadioKind::Ble,
            enabled: true,
            message: "Controller order: Switch 2 BLE discovery active".into(),
        });
    }
}

async fn sleep_all_for_controller_order(
    active: &mut HashMap<DeviceAddr, ActiveController>,
    events: &mpsc::Sender<Event>,
) {
    let addresses = active.keys().copied().collect::<Vec<_>>();
    for addr in addresses {
        let Some(mut controller) = active.remove(&addr) else {
            continue;
        };
        match &mut controller.transport {
            ActiveTransport::Ble(_) => {
                if let Err(error) = controller.sleep().await {
                    let _ = events.send(Event::Log(format!(
                        "Switch 2 sleep request for {addr} failed; disconnecting instead: {error}"
                    )));
                } else {
                    // The Nintendo sleep characteristic is write-without-response.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            ActiveTransport::BrEdr(_) if controller.identity.is_switch() => {
                if let Err(error) = controller.sleep().await {
                    let _ = events.send(Event::Log(format!(
                        "Nintendo sleep request for {addr} failed; disconnecting instead: {error}"
                    )));
                }
            }
            ActiveTransport::BrEdr(_) => {}
        }
        controller.report_task.abort();
        let _ = controller.transport.disconnect().await;
        let _ = events.send(Event::Disconnected {
            addr: addr.to_string(),
            reason: "Waiting to rejoin in controller order".into(),
        });
    }
}

async fn stop_active(active: &mut HashMap<DeviceAddr, ActiveController>, addr: DeviceAddr) {
    if let Some(mut controller) = active.remove(&addr) {
        controller.report_task.abort();
        let _ = controller.transport.disconnect().await;
    }
}

async fn disconnect_active(
    active: &mut HashMap<DeviceAddr, ActiveController>,
    addr: DeviceAddr,
    events: &mpsc::Sender<Event>,
) {
    let Some(mut controller) = active.remove(&addr) else {
        return;
    };
    if controller.identity.is_switch() && matches!(&controller.transport, ActiveTransport::BrEdr(_))
    {
        match controller.sleep().await {
            Ok(()) => {
                let _ = events.send(Event::Log(format!(
                    "Requested Nintendo controller {addr} sleep before disconnect"
                )));
            }
            Err(error) => {
                let _ = events.send(Event::Log(format!(
                    "Nintendo sleep request for {addr} failed: {error}"
                )));
            }
        }
    }
    controller.report_task.abort();
    let _ = controller.transport.disconnect().await;
    let _ = events.send(Event::Disconnected {
        addr: addr.to_string(),
        reason: "Disconnected".into(),
    });
}

async fn stop_all_active(active: &mut HashMap<DeviceAddr, ActiveController>) {
    let addresses = active.keys().copied().collect::<Vec<_>>();
    for addr in addresses {
        stop_active(active, addr).await;
    }
}

fn load_known_ble_controllers(store: &dyn ControllerStore) -> HashMap<DeviceAddr, KnownBle> {
    let Ok(records) = store.records() else {
        return HashMap::new();
    };
    records
        .into_iter()
        .filter_map(|record| {
            if record.transport != TransportKind::Ble
                || !record.profile_id.as_str().starts_with("nintendo.switch2.")
            {
                return None;
            }
            let controller = ControllerRegistry::standard().get(&record.profile_id)?;
            Some((
                record.address,
                KnownBle {
                    adapter_index: record.adapter_index.unwrap_or(0),
                    device: BleDevice {
                        addr: record.address,
                        name: Some(controller.display_name().into()),
                        rssi: None,
                        services: Vec::new(),
                        manufacturer_data: HashMap::new(),
                    },
                    profile_id: record.profile_id,
                    slot: usize::from(record.player_index.min(3)),
                },
            ))
        })
        .collect()
}

fn save_known_ble_controllers(
    store: &dyn ControllerStore,
    controllers: &HashMap<DeviceAddr, KnownBle>,
) -> polymer_bluetooth::Result<()> {
    for record in store.records()? {
        if record.transport == TransportKind::Ble
            && record.profile_id.as_str().starts_with("nintendo.switch2.")
            && !controllers.contains_key(&record.address)
        {
            store.remove(record.address)?;
        }
    }
    for known in controllers.values() {
        store.put(ControllerRecord {
            address: known.device.addr,
            transport: TransportKind::Ble,
            profile_id: known.profile_id.clone(),
            link_key: None,
            link_key_type: None,
            adapter_index: Some(known.adapter_index),
            auto_reconnect: true,
            player_index: u8::try_from(known.slot).unwrap_or(u8::MAX),
        })?;
    }
    Ok(())
}

fn load_controller_identities(store: &dyn ControllerStore) -> HashMap<DeviceAddr, BrIdentity> {
    store
        .records()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|record| {
            (record.transport == TransportKind::BrEdr)
                .then(|| {
                    BrIdentity::from_profile_id(record.profile_id.as_str())
                        .map(|identity| (record.address, identity))
                })
                .flatten()
        })
        .collect()
}

fn save_controller_identities(
    store: &dyn ControllerStore,
    identities: &HashMap<DeviceAddr, BrIdentity>,
) -> polymer_bluetooth::Result<()> {
    for (addr, identity) in identities {
        let Some(profile_id) = identity.token() else {
            continue;
        };
        let previous = store
            .records()?
            .into_iter()
            .find(|record| record.address == *addr);
        store.put(ControllerRecord {
            address: *addr,
            transport: TransportKind::BrEdr,
            profile_id: polymer_bluetooth::devices::ProfileId::new(profile_id),
            link_key: previous.as_ref().and_then(|record| record.link_key),
            link_key_type: previous.as_ref().and_then(|record| record.link_key_type),
            adapter_index: None,
            auto_reconnect: true,
            player_index: previous.as_ref().map_or(0, |record| record.player_index),
        })?;
    }
    Ok(())
}

fn diagnostic_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Polymer").join("bluetooth-test.log")
}

fn reset_diagnostic_log() {
    let path = diagnostic_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, "Polymer Bluetooth test diagnostics\n");
}

fn diagnostic(message: &str) {
    eprintln!("{message}");
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(diagnostic_path())
    {
        let _ = writeln!(file, "{message}");
    }
}

#[cfg(test)]
mod worker_tests {
    use super::*;

    #[test]
    fn bonded_ble_controller_registry_round_trips() {
        let store = MemoryControllerStore::default();
        let addr = "48:F1:EB:74:90:56".parse().unwrap();
        let mut original = HashMap::new();
        original.insert(
            addr,
            KnownBle {
                adapter_index: 2,
                device: BleDevice {
                    addr,
                    name: Some("Joy-Con 2 (R)".into()),
                    rssi: Some(-40),
                    services: Vec::new(),
                    manufacturer_data: HashMap::new(),
                },
                profile_id: ProfileId::new("nintendo.switch2.joycon-right"),
                slot: 1,
            },
        );

        save_known_ble_controllers(&store, &original).unwrap();
        let decoded = load_known_ble_controllers(&store);
        let restored = decoded.get(&addr).unwrap();
        assert_eq!(restored.adapter_index, 2);
        assert_eq!(restored.slot, 1);
        assert_eq!(
            restored.profile_id.as_str(),
            "nintendo.switch2.joycon-right"
        );
    }

    #[test]
    fn registry_round_trips_bredr_identity() {
        let store = MemoryControllerStore::default();
        let addr = "AA:BB:CC:DD:EE:FF".parse().unwrap();
        let mut identities = HashMap::new();
        identities.insert(
            addr,
            BrIdentity::new(Some(ProfileId::new("nintendo.wiimote.original"))),
        );
        save_controller_identities(&store, &identities).unwrap();
        assert_eq!(
            load_controller_identities(&store)
                .get(&addr)
                .unwrap()
                .token(),
            Some("nintendo.wiimote.original")
        );
    }
}
