use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use snowglass_bluetooth::{
    DeviceAddr, ReportReceiver, Transport,
    ble::{
        BleDevice, BleGatt, GattProfile, Switch2Kind, default_adapter_address, scan_switch2,
        stop_scan as stop_ble_adapter_scan,
    },
    bredr::{
        BrEdrHid, FileLinkKeyStore, PairingAgent, PinCode,
        hci::{AdapterSelector, Event as HciEvent, HciHost, InquiryResult},
        switch::{self, ControllerKind as SwitchControllerKind},
        wiimote,
    },
};
use tokio::{
    sync::{Mutex, mpsc as tokio_mpsc},
    task::JoinHandle,
};

const TP_LINK_VID: u16 = 0x2357;
const TP_LINK_PID: u16 = 0x0604;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinChoice {
    Ssp,
    WiiOneTwo,
    WiiSync,
}

impl PinChoice {
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RadioKind {
    BrEdr,
    Ble,
}

pub enum Command {
    BeginControllerOrder {
        adapter_index: usize,
    },
    EndControllerOrder,
    ConnectBr {
        device: InquiryResult,
        pin: PinChoice,
        is_wii_remote: bool,
        switch_kind: Option<SwitchControllerKind>,
    },
    ConnectBle {
        adapter_index: usize,
        device: BleDevice,
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
    WiiTelemetry {
        addr: String,
        telemetry: wiimote::Telemetry,
        received_at: Instant,
    },
    SwitchTelemetry {
        addr: String,
        telemetry: switch::Telemetry,
    },
    Log(String),
    Error(String),
}

enum ActiveTransport {
    BrEdr(BrEdrHid),
    Ble(Box<BleGatt>),
}

impl ActiveTransport {
    async fn send(&mut self, bytes: &[u8]) -> snowglass_bluetooth::Result<()> {
        match self {
            Self::BrEdr(transport) => transport.send(bytes).await,
            Self::Ble(_) => Err(snowglass_bluetooth::Error::Unsupported(
                "arbitrary Switch 2 BLE writes are disabled for controller safety",
            )),
        }
    }

    async fn disconnect(&mut self) -> snowglass_bluetooth::Result<()> {
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
    switch_packet_counter: u8,
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

struct InboundResult {
    addr: DeviceAddr,
    handle: u16,
    identity: BrIdentity,
    slot: usize,
    result: snowglass_bluetooth::Result<(BrEdrHid, ReportReceiver, u8)>,
}

struct OutboundResult {
    addr: DeviceAddr,
    identity: BrIdentity,
    slot: usize,
    result: snowglass_bluetooth::Result<(BrEdrHid, ReportReceiver, u8)>,
}

struct PendingBle {
    slot: usize,
    task: JoinHandle<()>,
}

struct BleResult {
    addr: DeviceAddr,
    slot: usize,
    adapter_index: usize,
    device: BleDevice,
    automatic: bool,
    result: snowglass_bluetooth::Result<(
        BleGatt,
        ReportReceiver,
        tokio::sync::broadcast::Receiver<()>,
    )>,
}

#[derive(Clone)]
struct KnownBle {
    adapter_index: usize,
    device: BleDevice,
    slot: usize,
}

struct BleDisconnected {
    addr: DeviceAddr,
    slot: usize,
}

struct BleRecovery {
    adapter_index: usize,
    device: BleDevice,
}

#[derive(Clone, Copy, Debug, Default)]
struct BrIdentity {
    is_wii_remote: bool,
    switch_kind: Option<SwitchControllerKind>,
}

impl BrIdentity {
    const fn new(is_wii_remote: bool, switch_kind: Option<SwitchControllerKind>) -> Self {
        Self {
            is_wii_remote,
            switch_kind,
        }
    }

    const fn token(self) -> Option<&'static str> {
        if self.is_wii_remote {
            return Some("wii");
        }
        match self.switch_kind {
            Some(SwitchControllerKind::ProController) => Some("switch-pro"),
            Some(SwitchControllerKind::JoyConLeft) => Some("joycon-left"),
            Some(SwitchControllerKind::JoyConRight) => Some("joycon-right"),
            Some(SwitchControllerKind::NsoNes) => Some("nso-nes"),
            Some(SwitchControllerKind::NsoSnes) => Some("nso-snes"),
            Some(SwitchControllerKind::NsoN64) => Some("nso-n64"),
            Some(SwitchControllerKind::NsoGenesis) => Some("nso-genesis"),
            None => None,
        }
    }

    const fn label(self) -> &'static str {
        if self.is_wii_remote {
            return "Wii Remote";
        }
        match self.switch_kind {
            Some(kind) => kind.label(),
            None => "BR/EDR controller",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        let switch_kind = match token {
            "wii" => return Some(Self::new(true, None)),
            "switch-pro" => SwitchControllerKind::ProController,
            "joycon-left" => SwitchControllerKind::JoyConLeft,
            "joycon-right" => SwitchControllerKind::JoyConRight,
            "nso-nes" => SwitchControllerKind::NsoNes,
            "nso-snes" => SwitchControllerKind::NsoSnes,
            "nso-n64" => SwitchControllerKind::NsoN64,
            "nso-genesis" => SwitchControllerKind::NsoGenesis,
            _ => return None,
        };
        Some(Self::new(false, Some(switch_kind)))
    }
}

pub fn spawn() -> (tokio_mpsc::UnboundedSender<Command>, mpsc::Receiver<Event>) {
    reset_diagnostic_log();
    diagnostic("Starting Snowglass Bluetooth test worker");
    let (command_tx, command_rx) = tokio_mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::channel();
    thread::Builder::new()
        .name("snowglass-bttest".into())
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

    let selector = AdapterSelector::from_vid_pid(TP_LINK_VID, TP_LINK_PID);
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
    let pairing = if let Some(host) = &host {
        match FileLinkKeyStore::open(key_store_path()) {
            Ok(store) => Some(Arc::new(PairingAgent::new(host.clone(), Arc::new(store)))),
            Err(error) => {
                let _ = events.send(Event::Log(format!("link-key store unavailable: {error}")));
                None
            }
        }
    } else {
        None
    };
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
            diagnostic(&format!("Loaded RTL8761BU firmware from {source}"));
            let _ = events.send(Event::Log(format!(
                "Initialized TP-Link radio firmware from {}",
                PathBuf::from(source)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(source)
            )));
        }
        let _ = events.send(Event::BrReady(format!(
            "TP-Link {} · ACL {} bytes / {} buffers",
            info.address, info.acl_data_packet_length, info.total_acl_packets
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
    let mut known_ble = load_known_ble_controllers();
    let mut restored_ble = known_ble.values().cloned().collect::<Vec<_>>();
    restored_ble.sort_by_key(|known| known.slot);
    for known in restored_ble {
        let addr = known.device.addr;
        diagnostic(&format!(
            "Restored bonded Switch 2 controller {addr} ({}) in player slot {}; waiting for wake advertisement",
            known.device.kind.label(),
            known.slot + 1
        ));
        let _ = events.send(Event::Log(format!(
            "Bonded {} restored in player slot {}; press a controller button to reconnect",
            known.device.kind.label(),
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
    let mut identities = load_controller_identities();
    let mut listen_inbound = true;
    let _ = events.send(Event::Log(
        "Inbound reconnects enabled for known bonded controllers".into(),
    ));
    loop {
        tokio::select! {
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
                    device: recovery.device,
                    slot,
                };
                diagnostic(&format!(
                    "Recovered existing Switch 2 bond for {addr} ({}) in player slot {}",
                    known.device.kind.label(),
                    slot + 1
                ));
                let _ = events.send(Event::Log(format!(
                    "Recovered bonded {} at {addr}; connecting…",
                    known.device.kind.label()
                )));
                known_ble.insert(addr, known.clone());
                if let Err(error) = save_known_ble_controllers(&known_ble) {
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
                    if let Ok((mut transport, _, _)) = completion.result {
                        let _ = transport.disconnect().await;
                    }
                    continue;
                }
                pending_ble.remove(&completion.addr);
                match completion.result {
                    Ok((transport, mut reports, mut disconnects)) => {
                        known_ble.insert(
                            completion.addr,
                            KnownBle {
                                adapter_index: completion.adapter_index,
                                device: completion.device.clone(),
                                slot: completion.slot,
                            },
                        );
                        if let Err(error) = save_known_ble_controllers(&known_ble) {
                            let _ = events.send(Event::Log(format!(
                                "Could not persist bonded Switch 2 controller: {error}"
                            )));
                        }
                        stop_active(&mut active, completion.addr).await;
                        let report_events = events.clone();
                        let disconnect_tx = ble_disconnect_tx.clone();
                        let addr = completion.addr;
                        let slot = completion.slot;
                        let report_task = tokio::spawn(async move {
                            loop {
                                tokio::select! {
                                    result = reports.recv() => match result {
                                        Ok(report) => {
                                            let _ = report_events.send(Event::Report {
                                                addr: addr.to_string(),
                                                bytes: report.to_vec(),
                                            });
                                        }
                                        Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                                            let _ = report_events.send(Event::Log(format!(
                                                "Dropped {count} raw BLE reports from {addr}"
                                            )));
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
                            identity: BrIdentity::default(),
                            slot: completion.slot,
                            switch_packet_counter: 0,
                        });
                        diagnostic(&format!(
                            "Switch 2 GATT notifications and wake reconnect ready for {} in player slot {}",
                            completion.addr,
                            completion.slot + 1
                        ));
                        let _ = events.send(Event::Connected {
                            addr: completion.addr.to_string(),
                            name: completion.device.kind.label().into(),
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
                    if let Ok((mut transport, _, _)) = completion.result {
                        let _ = transport.disconnect().await;
                    }
                    continue;
                }
                pending_outbound.remove(&completion.addr);
                match completion.result {
                    Ok((transport, reports, switch_packet_counter)) => {
                        listen_inbound = true;
                        if completion.identity.token().is_some() {
                            identities.insert(completion.addr, completion.identity);
                            if let Err(error) = save_controller_identities(&identities) {
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
                            completion.identity,
                        );
                        active.insert(
                            completion.addr,
                            ActiveController {
                                transport: ActiveTransport::BrEdr(transport),
                                report_task,
                                identity: completion.identity,
                                slot: completion.slot,
                                switch_packet_counter,
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
                    if let Ok((mut transport, _, _)) = completion.result {
                        let _ = transport.disconnect().await;
                    }
                    continue;
                }
                pending_inbound.remove(&completion.addr);
                match completion.result {
                    Ok((transport, reports, switch_packet_counter)) => {
                        stop_active(&mut active, completion.addr).await;
                        let report_task = spawn_bredr_report_task(
                            reports,
                            events.clone(),
                            completion.addr,
                            completion.identity,
                        );
                        active.insert(
                            completion.addr,
                            ActiveController {
                                transport: ActiveTransport::BrEdr(transport),
                                report_task,
                                identity: completion.identity,
                                slot: completion.slot,
                                switch_packet_counter,
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
                        if completion.identity.switch_kind.is_some() {
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
                        mut is_wii_remote,
                        mut switch_kind,
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
                        if !is_wii_remote && switch_kind.is_none() {
                            match host.remote_name(&device).await {
                                Ok(name) => {
                                    is_wii_remote = name.starts_with("Nintendo RVL-CNT");
                                    switch_kind = SwitchControllerKind::from_name(&name);
                                    diagnostic(&format!(
                                        "Resolved {} as {name} before connecting",
                                        device.addr
                                    ));
                                    let _ = events.send(Event::Device(device_view(
                                        device.clone(),
                                        name.clone(),
                                    )));
                                    let _ = events.send(Event::Log(format!(
                                        "Resolved {} as {name}",
                                        device.addr
                                    )));
                                }
                                Err(error) => {
                                    diagnostic(&format!(
                                        "Final remote-name request for {} failed: {error}",
                                        device.addr
                                    ));
                                    let _ = events.send(Event::Log(format!(
                                        "Could not identify {} before connecting: {error}",
                                        device.addr
                                    )));
                                }
                            }
                        }
                        if switch_kind.is_some() {
                            if let Err(error) = pairing.forget(device.addr) {
                                let _ = events.send(Event::Error(format!(
                                    "could not clear the previous Nintendo bond: {error}"
                                )));
                                continue;
                            }
                            diagnostic(&format!(
                                "Forcing fresh SSP pairing for Nintendo controller {}",
                                device.addr
                            ));
                            let _ = events.send(Event::Log(
                                "Cleared the previous Nintendo bond for fresh SSP pairing".into(),
                            ));
                        }
                        if let Err(error) = configure_pin(pairing, device.addr, pin, switch_kind) {
                            let _ = events.send(Event::Error(error.to_string()));
                            continue;
                        }
                        let pin_source = match pin {
                            PinChoice::Ssp if switch_kind.is_some() => {
                                "SSP Just Works; legacy fallback PIN = 0000".to_owned()
                            }
                            PinChoice::Ssp => "SSP / no legacy PIN".to_owned(),
                            PinChoice::WiiOneTwo => {
                                format!("temporary PIN = reverse of Wii address {}", device.addr)
                            }
                            PinChoice::WiiSync => format!(
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
                        if is_wii_remote {
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
                        let identity = BrIdentity::new(is_wii_remote, switch_kind);
                        let completion_tx = outbound_tx.clone();
                        let task_events = events.clone();
                        let task = tokio::spawn(async move {
                            let result = async {
                                // During fresh SSP pairing the host opens both HID
                                // PSMs. Bonded button-wake reconnects use the opposite,
                                // controller-initiated direction handled below.
                                transport.connect_discovered(&device).await?;

                                let switch_packet_counter = if is_wii_remote {
                                    if let Err(error) = wiimote::initialize(&mut transport, slot).await {
                                        let _ = transport.disconnect().await;
                                        return Err(error);
                                    }
                                    diagnostic(&format!(
                                        "Initialized Wii Remote {addr}: player LED {}, continuous buttons+accelerometer+IR+MotionPlus reports",
                                        slot + 1
                                    ));
                                    let _ = task_events.send(Event::Log(format!(
                                        "Wii Remote player LED {}, IR camera, MotionPlus, and continuous input enabled",
                                        slot + 1
                                    )));
                                    0
                                } else if let Some(kind) = switch_kind {
                                    let packet_counter = match switch::initialize(&mut transport, kind, slot).await {
                                        Ok(packet_counter) => packet_counter,
                                        Err(error) => {
                                            let _ = transport.disconnect().await;
                                            return Err(error);
                                        }
                                    };
                                    diagnostic(&format!(
                                        "Initialized {} {addr}: native 0x30 reports, vibration, player LED {}",
                                        kind.label(),
                                        slot + 1
                                    ));
                                    let _ = task_events.send(Event::Log(format!(
                                        "{} native input enabled in player slot {}",
                                        kind.label(),
                                        slot + 1
                                    )));
                                    packet_counter
                                } else {
                                    0
                                };
                                Ok((transport, reports, switch_packet_counter))
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
                        let automatic = controller_order_adapter.is_none()
                            && known_ble.contains_key(&addr)
                            && !device.is_pairing_mode();
                        let _ = events.send(Event::Busy(format!("Connecting to {} over BLE…", device.kind.label())));
                        diagnostic(&format!(
                            "Starting Switch 2 BLE connection to {addr} ({}) on OS adapter {adapter_index}",
                            device.kind.label()
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
                                if let Err(error) = save_controller_identities(&identities) {
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
                        let sleep_result = match &mut controller.transport {
                            ActiveTransport::Ble(transport) => transport.sleep_switch2().await,
                            ActiveTransport::BrEdr(_) => Err(snowglass_bluetooth::Error::Unsupported(
                                "Switch 2 sleep is only available for BLE sessions",
                            )),
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
                        let Some(identity) = identities.get(&addr).copied() else {
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
                        let become_master = identity.switch_kind.is_none();
                        match host.accept_connection_with_role(addr, become_master).await {
                            Ok(handle) => {
                                let mut transport = BrEdrHid::new(host.clone());
                                if identity.is_wii_remote {
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
                                        if identity.switch_kind.is_some() {
                                            transport
                                                .attach_inbound_switch_with_frames(
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

                                        let switch_packet_counter = if identity.is_wii_remote {
                                            wiimote::initialize(&mut transport, slot).await?;
                                            let _ = task_events.send(Event::Log(format!(
                                                "Reconnected Wii Remote in player slot {}",
                                                slot + 1
                                            )));
                                            0
                                        } else if let Some(kind) = identity.switch_kind {
                                            let packet_counter =
                                                switch::initialize(&mut transport, kind, slot).await?;
                                            diagnostic(&format!(
                                                "Reconnected {} {addr}: native 0x30 reports and player LED {}",
                                                kind.label(),
                                                slot + 1
                                            ));
                                            let _ = task_events.send(Event::Log(format!(
                                                "Reconnected {} in player slot {} with native input",
                                                kind.label(),
                                                slot + 1
                                            )));
                                            packet_counter
                                        } else {
                                            0
                                        };
                                        Ok((transport, reports, switch_packet_counter))
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
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match reports.recv().await {
                Ok(report) => {
                    if identity.is_wii_remote
                        && let Some(telemetry) = wiimote::parse_report(&report)
                    {
                        let _ = events.send(Event::WiiTelemetry {
                            addr: addr.to_string(),
                            telemetry,
                            received_at: Instant::now(),
                        });
                    }
                    if let Some(kind) = identity.switch_kind
                        && let Some(telemetry) = switch::parse_report(kind, &report)
                    {
                        let _ = events.send(Event::SwitchTelemetry {
                            addr: addr.to_string(),
                            telemetry,
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
    // Pair exactly once, from an explicit user-selected pairing advertisement.
    // A wake attempt retains that original advertisement as slot metadata, so
    // consulting it again would overwrite a valid bond on every reconnect.
    let pairing_mode = !automatic && device.is_pairing_mode();
    let result_device = device.clone();
    let task = tokio::spawn(async move {
        let mut transport = BleGatt::new(adapter_index, GattProfile::switch2_for(device.kind));
        if automatic {
            // Rotate the single adapter fairly between dormant controllers.
            // A Switch 2 wake advertisement persists across several windows.
            transport.set_scan_timeout(Duration::from_secs(3));
        }
        let reports = transport.reports();
        let disconnects = transport.disconnections();
        let result = loop {
            let connection = {
                let _scan_guard = scan_gate.lock().await;
                transport.connect(addr).await
            };
            match connection {
                Ok(()) => {
                    if pairing_mode {
                        let bond_result = async {
                            let host_address = default_adapter_address().await?;
                            transport.bond_switch2(host_address).await
                        }
                        .await;
                        match bond_result {
                            Ok(bond) => {
                                diagnostic(&format!(
                                    "Committed Switch 2 bond for {addr} to OS adapter {}",
                                    bond.host_address
                                ));
                                let _ = events.send(Event::Log(format!(
                                    "Bonded {addr} to {}; normal-button wake is enabled",
                                    bond.host_address
                                )));
                            }
                            Err(error) => {
                                let _ = transport.disconnect().await;
                                break Err(error);
                            }
                        }
                    }
                    if let Err(error) = transport.set_switch2_player_led(slot).await {
                        let _ = transport.disconnect().await;
                        break Err(error);
                    }
                    if device.kind != Switch2Kind::NsoGameCube
                        && let Err(error) = transport.set_switch2_led_flashing(false).await
                    {
                        let _ = transport.disconnect().await;
                        break Err(error);
                    }
                    diagnostic(&format!(
                        "Switch 2 controller {addr} acknowledged player LED {}{}",
                        slot + 1,
                        if device.kind == Switch2Kind::NsoGameCube {
                            " (GameCube flash state left untouched)"
                        } else {
                            " with flashing disabled"
                        },
                    ));
                    break Ok((transport, reports, disconnects));
                }
                Err(error) if !automatic => break Err(error),
                Err(snowglass_bluetooth::Error::Unsupported(reason)) => {
                    break Err(snowglass_bluetooth::Error::Unsupported(reason));
                }
                Err(snowglass_bluetooth::Error::Backoff(delay)) => {
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
        };
        let _ = completion_tx.send(BleResult {
            addr,
            slot,
            adapter_index,
            device: result_device,
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
                scan_switch2(adapter_index, Duration::from_secs(4)).await
            };
            match result {
                Ok(found) => {
                    if let Some(device) = found
                        .into_iter()
                        .find(|device| device.is_bonded_to(host_address))
                    {
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
        let mut use_wii_liac = true;
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
    DeviceView {
        addr: inquiry.addr.to_string(),
        name,
        rssi: inquiry.rssi,
        class: format!(
            "{:02X}{:02X}{:02X}",
            inquiry.class_of_device[2], inquiry.class_of_device[1], inquiry.class_of_device[0]
        ),
        inquiry,
    }
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
                scan_switch2(adapter_index, Duration::from_secs(4)).await
            };
            match result {
                Ok(found) => {
                    for device in found {
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
    choice: PinChoice,
    switch_kind: Option<SwitchControllerKind>,
) -> snowglass_bluetooth::Result<()> {
    match choice {
        PinChoice::Ssp if switch_kind.is_some() => {
            pairing.set_pin(addr, PinCode::Fixed(b"0000".to_vec()))
        }
        PinChoice::Ssp => pairing.clear_pin(addr),
        PinChoice::WiiOneTwo => pairing.set_pin(addr, PinCode::RemoteAddress),
        PinChoice::WiiSync => pairing.set_pin(addr, PinCode::LocalAddress),
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
            ActiveTransport::Ble(transport) => {
                if let Err(error) = transport.sleep_switch2().await {
                    let _ = events.send(Event::Log(format!(
                        "Switch 2 sleep request for {addr} failed; disconnecting instead: {error}"
                    )));
                } else {
                    // The Nintendo sleep characteristic is write-without-response.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            ActiveTransport::BrEdr(transport) if controller.identity.switch_kind.is_some() => {
                if let Err(error) =
                    switch::request_sleep(transport, &mut controller.switch_packet_counter).await
                {
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
    if controller.identity.switch_kind.is_some()
        && let ActiveTransport::BrEdr(transport) = &mut controller.transport
    {
        match switch::request_sleep(transport, &mut controller.switch_packet_counter).await {
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

fn key_store_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Snowglass").join("bluetooth-link-keys.txt")
}

fn identity_store_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Snowglass")
        .join("bluetooth-controller-identities.txt")
}

fn ble_controller_store_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Snowglass").join("bluetooth-ble-controllers.txt")
}

fn switch2_kind_token(kind: Switch2Kind) -> &'static str {
    match kind {
        Switch2Kind::JoyConLeft => "joycon2-left",
        Switch2Kind::JoyConRight => "joycon2-right",
        Switch2Kind::ProController => "pro2",
        Switch2Kind::NsoGameCube => "nso-gamecube2",
        Switch2Kind::Unknown => "switch2-unknown",
    }
}

fn switch2_kind_from_token(token: &str) -> Option<Switch2Kind> {
    match token {
        "joycon2-left" => Some(Switch2Kind::JoyConLeft),
        "joycon2-right" => Some(Switch2Kind::JoyConRight),
        "pro2" => Some(Switch2Kind::ProController),
        "nso-gamecube2" => Some(Switch2Kind::NsoGameCube),
        "switch2-unknown" => Some(Switch2Kind::Unknown),
        _ => None,
    }
}

fn parse_known_ble_controllers(contents: &str) -> HashMap<DeviceAddr, KnownBle> {
    let mut controllers = HashMap::new();
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let (Some(addr), Some(adapter), Some(slot), Some(kind)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(addr), Ok(adapter_index), Ok(slot), Some(kind)) = (
            addr.parse(),
            adapter.parse::<usize>(),
            slot.parse::<usize>(),
            switch2_kind_from_token(kind),
        ) else {
            continue;
        };
        let slot = slot.min(3);
        controllers.insert(
            addr,
            KnownBle {
                adapter_index,
                device: BleDevice {
                    addr,
                    name: Some(kind.label().into()),
                    rssi: None,
                    kind,
                    // A restored entry is already bonded. Empty advertisement
                    // data also guarantees startup wake cannot pair it again.
                    manufacturer_data: HashMap::new(),
                },
                slot,
            },
        );
    }
    controllers
}

fn serialize_known_ble_controllers(controllers: &HashMap<DeviceAddr, KnownBle>) -> String {
    let mut entries = controllers.values().collect::<Vec<_>>();
    entries.sort_by_key(|known| known.device.addr.bytes());
    let mut contents = String::from("# Snowglass bonded Switch 2 controllers\n");
    for known in entries {
        contents.push_str(&format!(
            "{} {} {} {}\n",
            known.device.addr,
            known.adapter_index,
            known.slot,
            switch2_kind_token(known.device.kind)
        ));
    }
    contents
}

fn load_known_ble_controllers() -> HashMap<DeviceAddr, KnownBle> {
    let Ok(contents) = fs::read_to_string(ble_controller_store_path()) else {
        return HashMap::new();
    };
    parse_known_ble_controllers(&contents)
}

fn save_known_ble_controllers(
    controllers: &HashMap<DeviceAddr, KnownBle>,
) -> snowglass_bluetooth::Result<()> {
    let path = ble_controller_store_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serialize_known_ble_controllers(controllers))?;
    Ok(())
}

fn load_controller_identities() -> HashMap<DeviceAddr, BrIdentity> {
    let mut identities = HashMap::new();
    let Ok(contents) = fs::read_to_string(identity_store_path()) else {
        return identities;
    };
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let (Some(addr), Some(token)) = (fields.next(), fields.next()) else {
            continue;
        };
        if let (Ok(addr), Some(identity)) = (addr.parse(), BrIdentity::from_token(token)) {
            identities.insert(addr, identity);
        }
    }
    identities
}

fn save_controller_identities(
    identities: &HashMap<DeviceAddr, BrIdentity>,
) -> snowglass_bluetooth::Result<()> {
    let path = identity_store_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut entries: Vec<_> = identities
        .iter()
        .filter_map(|(addr, identity)| identity.token().map(|token| (*addr, token)))
        .collect();
    entries.sort_by_key(|(addr, _)| addr.bytes());
    let mut contents = String::from("# Snowglass BR/EDR controller identities\n");
    for (addr, token) in entries {
        contents.push_str(&format!("{addr} {token}\n"));
    }
    fs::write(path, contents)?;
    Ok(())
}

fn diagnostic_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Snowglass").join("bluetooth-test.log")
}

fn reset_diagnostic_log() {
    let path = diagnostic_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, "Snowglass Bluetooth test diagnostics\n");
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
                    kind: Switch2Kind::JoyConRight,
                    manufacturer_data: HashMap::new(),
                },
                slot: 1,
            },
        );

        let encoded = serialize_known_ble_controllers(&original);
        let decoded = parse_known_ble_controllers(&encoded);
        let restored = decoded.get(&addr).unwrap();
        assert_eq!(restored.adapter_index, 2);
        assert_eq!(restored.slot, 1);
        assert_eq!(restored.device.kind, Switch2Kind::JoyConRight);
        assert!(!restored.device.is_pairing_mode());
    }

    #[test]
    fn invalid_registry_lines_are_ignored_and_slots_are_bounded() {
        let decoded = parse_known_ble_controllers(
            "garbage\n48:F1:EB:74:90:56 0 99 joycon2-right\nAA:BB:CC:DD:EE:FF x 0 pro2\n",
        );
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded.values().next().unwrap().slot, 3);
    }
}
