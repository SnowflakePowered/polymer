//! High-level Bluetooth discovery and controller lifecycle manager.

use std::{collections::HashMap, sync::Arc, time::Duration};

use bytes::Bytes;
use serde_json::Value;
use tokio::{
    sync::{broadcast, mpsc, oneshot},
    task::JoinHandle,
};

use crate::{
    DeviceAddr, Error, Result, Transport, TransportKind,
    devices::{
        ConnectOptions, ControllerProfile, DeviceIdentity, PairingPolicy, ProfileId,
        ProfileRegistry,
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
    pub profiles: ProfileRegistry,
    pub firmware: FirmwareRegistry,
    pub store: Arc<dyn ControllerStore>,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            bredr_adapter: AdapterSelector::first_winusb(),
            ble_adapter_index: 0,
            profiles: ProfileRegistry::standard(),
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
        address: DeviceAddr,
        profile_id: ProfileId,
    },
    Connected {
        address: DeviceAddr,
        profile_id: ProfileId,
        player_index: u8,
    },
    RawReport {
        address: DeviceAddr,
        bytes: Bytes,
    },
    Disconnected {
        address: DeviceAddr,
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
    StopScan(TransportKind, oneshot::Sender<Result<()>>),
    Connect(DeviceAddr, ConnectOptions, oneshot::Sender<Result<()>>),
    Send(DeviceAddr, Vec<u8>, oneshot::Sender<Result<()>>),
    Disconnect(DeviceAddr, bool, oneshot::Sender<Result<()>>),
    Forget(DeviceAddr, oneshot::Sender<Result<()>>),
    WakeDiscovered(ControllerRecord, BleDevice),
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
            run_manager(config, receiver, internal, actor_events).await;
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
    pub async fn send_raw(&self, address: DeviceAddr, bytes: &[u8]) -> Result<()> {
        self.call(|reply| Command::Send(address, bytes.to_vec(), reply))
            .await
    }
    pub async fn sleep(&self, address: DeviceAddr) -> Result<()> {
        self.call(|reply| Command::Disconnect(address, true, reply))
            .await
    }
    pub async fn disconnect(&self, address: DeviceAddr) -> Result<()> {
        self.call(|reply| Command::Disconnect(address, false, reply))
            .await
    }
    pub async fn forget(&self, address: DeviceAddr) -> Result<()> {
        self.call(|reply| Command::Forget(address, reply)).await
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
        self.commands
            .send(build(tx))
            .await
            .map_err(|_| Error::ChannelClosed)?;
        rx.await.map_err(|_| Error::ChannelClosed)?
    }
}

struct Discovered {
    identity: DeviceIdentity,
    bredr: Option<InquiryResult>,
    ble: Option<BleDevice>,
}

struct Session {
    transport: Box<dyn Transport>,
    profile: Arc<dyn ControllerProfile>,
    state: Value,
    report_task: JoinHandle<()>,
}

async fn run_manager(
    config: ManagerConfig,
    mut commands: mpsc::Receiver<Command>,
    internal: mpsc::Sender<Command>,
    events: broadcast::Sender<ManagerEvent>,
) {
    let bredr =
        match HciHost::open_with_firmware(&config.bredr_adapter, config.firmware.clone()).await {
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
    match BleGatt::adapter_names().await {
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
    let mut sessions = HashMap::<DeviceAddr, Session>::new();
    let mut hci_events = bredr.as_ref().map(|host| host.events());
    let wake_gate = Arc::new(tokio::sync::Mutex::new(()));
    let mut wake_tasks = HashMap::<DeviceAddr, JoinHandle<()>>::new();
    for record in config.store.records().unwrap_or_default() {
        if config.profiles.get(&record.profile_id).is_none() {
            let _ = events.send(ManagerEvent::Error {
                address: Some(record.address),
                message: Error::MissingProfile(record.profile_id.to_string()).to_string(),
            });
        } else if record.transport == TransportKind::Ble && record.auto_reconnect {
            arm_ble_wake(
                record,
                config.profiles.clone(),
                internal.clone(),
                wake_gate.clone(),
                &mut wake_tasks,
            );
        }
    }

    loop {
        let command = tokio::select! {
            command = commands.recv() => command,
            event = receive_hci(&mut hci_events) => {
                match event {
                    Some(Ok(HciEvent::ConnectionRequest { addr, .. })) => {
                        if sessions.contains_key(&addr) { continue; }
                        match accept_inbound(&config, bredr.as_ref().unwrap(), addr, &events).await {
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
                let result = scan(
                    &config,
                    bredr.as_ref(),
                    request,
                    &events,
                    &mut discovered,
                    &wake_gate,
                )
                .await;
                let _ = reply.send(result);
            }
            Command::StopScan(kind, reply) => {
                let result = match kind {
                    TransportKind::Ble => {
                        let _guard = wake_gate.lock().await;
                        stop_scan(config.ble_adapter_index).await
                    }
                    TransportKind::BrEdr => match &bredr {
                        Some(host) => host.cancel_inquiry().await,
                        None => Err(Error::AdapterNotFound),
                    },
                };
                let _ = reply.send(result);
            }
            Command::Connect(address, options, reply) => {
                let result = connect(
                    ConnectContext {
                        config: &config,
                        host: bredr.as_ref(),
                        pairing: pairing.as_ref(),
                        keys: keys.as_ref(),
                        events: &events,
                    },
                    address,
                    options,
                    &discovered,
                    &mut sessions,
                )
                .await;
                let _ = reply.send(result);
            }
            Command::Send(address, bytes, reply) => {
                let result = match sessions.get_mut(&address) {
                    Some(session) => {
                        session
                            .profile
                            .device()
                            .send_raw_hid(session.transport.as_mut(), &bytes)
                            .await
                    }
                    None => Err(Error::Unsupported("controller is not connected")),
                };
                let _ = reply.send(result);
            }
            Command::Disconnect(address, sleep, reply) => {
                let result = disconnect(address, sleep, &events, &mut sessions).await;
                if sleep
                    && result.is_ok()
                    && let Some(record) = config
                        .store
                        .records()
                        .unwrap_or_default()
                        .into_iter()
                        .find(|record| {
                            record.address == address
                                && record.transport == TransportKind::Ble
                                && record.auto_reconnect
                        })
                {
                    arm_ble_wake(
                        record,
                        config.profiles.clone(),
                        internal.clone(),
                        wake_gate.clone(),
                        &mut wake_tasks,
                    );
                }
                let _ = reply.send(result);
            }
            Command::Forget(address, reply) => {
                if let Some(task) = wake_tasks.remove(&address) {
                    task.abort();
                }
                let _ = disconnect(address, false, &events, &mut sessions).await;
                if let Some(pairing) = &pairing {
                    let _ = pairing.forget(address);
                }
                let _ = reply.send(config.store.remove(address));
            }
            Command::WakeDiscovered(record, device) => {
                wake_tasks.remove(&record.address);
                let identity = ble_identity(&device);
                let publish = publish_discovery(
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
                        connect(
                            ConnectContext {
                                config: &config,
                                host: bredr.as_ref(),
                                pairing: pairing.as_ref(),
                                keys: keys.as_ref(),
                                events: &events,
                            },
                            record.address,
                            options,
                            &discovered,
                            &mut sessions,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                };
                if let Err(error) = result {
                    let _ = events.send(ManagerEvent::Disconnected {
                        address: record.address,
                        reason: error.to_string(),
                        reconnecting: true,
                    });
                    arm_ble_wake(
                        record,
                        config.profiles.clone(),
                        internal.clone(),
                        wake_gate.clone(),
                        &mut wake_tasks,
                    );
                }
            }
            Command::Shutdown(reply) => {
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

fn arm_ble_wake(
    record: ControllerRecord,
    profiles: ProfileRegistry,
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
                        let profile = profiles.get(&record.profile_id)?;
                        devices.into_iter().find(|device| {
                            device.addr == address
                                && profile.device().accepts_reconnect_advertisement(
                                    &ble_identity(device),
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
                let identity = ble_identity(&device);
                publish_discovery(config, identity, None, Some(device), events, discovered)?;
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
                publish_discovery(config, identity, Some(inquiry), None, events, discovered)?;
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
        .profiles
        .identify(&identity)?
        .map(|profile| profile.id());
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

struct ConnectContext<'a> {
    config: &'a ManagerConfig,
    host: Option<&'a Arc<HciHost>>,
    pairing: Option<&'a Arc<PairingAgent>>,
    keys: &'a ControllerLinkKeyStore,
    events: &'a broadcast::Sender<ManagerEvent>,
}

async fn connect(
    context: ConnectContext<'_>,
    address: DeviceAddr,
    options: ConnectOptions,
    discovered: &HashMap<DeviceAddr, Discovered>,
    sessions: &mut HashMap<DeviceAddr, Session>,
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
        let preliminary = config.profiles.identify(&identity)?;
        if preliminary
            .as_ref()
            .is_none_or(|profile| profile.id().as_str() == "generic.hid")
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
    let profile = config
        .profiles
        .identify(&identity)?
        .ok_or(Error::Unsupported(
            "no controller profile matched the device",
        ))?;
    let profile_id = profile.id();
    let _ = events.send(ManagerEvent::Connecting {
        address,
        profile_id: profile_id.clone(),
    });
    let policy = profile.connection_policy(&identity, &options);
    let mut state = Value::Object(Default::default());
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
            let gatt_profile = profile
                .device()
                .gatt_profile(&identity, &mut state)?
                .ok_or(Error::Unsupported(
                    "controller profile does not provide a BLE GATT configuration",
                ))?;
            let mut gatt = BleGatt::new(config.ble_adapter_index, gatt_profile);
            gatt.connect(address).await?;
            if matches!(policy.pairing, PairingPolicy::PseudoOob) {
                let host_address = default_adapter_address().await?;
                profile
                    .device()
                    .bond(&mut gatt, &identity, host_address, &mut state)
                    .await?;
            }
            Box::new(gatt)
        }
    };
    profile
        .device()
        .sync(transport.as_mut(), &options, &mut state)
        .await?;
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
        profile_state: state.clone(),
    })?;
    let report_task = spawn_reports(address, transport.reports(), events.clone());
    sessions.insert(
        address,
        Session {
            transport,
            profile,
            state,
            report_task,
        },
    );
    let _ = events.send(ManagerEvent::Connected {
        address,
        profile_id,
        player_index: options.player_index,
    });
    Ok(())
}

async fn accept_inbound(
    config: &ManagerConfig,
    host: &Arc<HciHost>,
    address: DeviceAddr,
    events: &broadcast::Sender<ManagerEvent>,
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
    let profile = config
        .profiles
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
    let policy = profile.connection_policy(&identity, &options);
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
    let mut state = record.profile_state;
    if let Err(error) = profile
        .device()
        .sync(transport.as_mut(), &options, &mut state)
        .await
    {
        let _ = transport.disconnect().await;
        return Err(error);
    }
    let report_task = spawn_reports(address, transport.reports(), events.clone());
    let _ = events.send(ManagerEvent::Connected {
        address,
        profile_id: record.profile_id,
        player_index: record.player_index,
    });
    Ok(Session {
        transport,
        profile,
        state,
        report_task,
    })
}

fn spawn_reports(
    address: DeviceAddr,
    mut reports: crate::ReportReceiver,
    events: broadcast::Sender<ManagerEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match reports.recv().await {
                Ok(bytes) => {
                    let _ = events.send(ManagerEvent::RawReport { address, bytes });
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

async fn receive_hci(
    receiver: &mut Option<broadcast::Receiver<HciEvent>>,
) -> Option<std::result::Result<HciEvent, broadcast::error::RecvError>> {
    match receiver {
        Some(receiver) => Some(receiver.recv().await),
        None => std::future::pending().await,
    }
}

async fn disconnect(
    address: DeviceAddr,
    sleep: bool,
    events: &broadcast::Sender<ManagerEvent>,
    sessions: &mut HashMap<DeviceAddr, Session>,
) -> Result<()> {
    let Some(mut session) = sessions.remove(&address) else {
        return Ok(());
    };
    if sleep {
        session
            .profile
            .device()
            .sleep(session.transport.as_mut(), &mut session.state)
            .await?;
    }
    session.report_task.abort();
    session.transport.disconnect().await?;
    let _ = events.send(ManagerEvent::Disconnected {
        address,
        reason: if sleep {
            "sleep requested"
        } else {
            "disconnected"
        }
        .into(),
        reconnecting: false,
    });
    Ok(())
}
