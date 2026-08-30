use parking_lot::RwLock;
use snowglass_bluetooth::{
    ControllerId, ManagerConfig, ManagerEvent, ScanRequest, TransportKind, devices::ConnectOptions,
};
use snowglass_input::{
    router::PhysicalInputRouter,
    source::{InputSourceCatalog, InputSourceDescriptor, InputSourceKind},
};
use snowglass_input_bluetooth::{
    BluetoothControllerState, BluetoothInputProvider, BluetoothProviderEvent, source_id,
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone)]
pub(super) struct BluetoothHandle {
    commands: mpsc::UnboundedSender<BluetoothCommand>,
    snapshot: Arc<RwLock<serde_json::Value>>,
    controllers: Arc<RwLock<Vec<BluetoothControllerState>>>,
}

enum BluetoothCommand {
    Api {
        value: serde_json::Value,
        result: oneshot::Sender<Result<serde_json::Value, String>>,
    },
    SessionActive(bool),
    AssignPlayers(Vec<(String, u8)>),
    DisconnectSources(Vec<String>),
}

struct BluetoothActor {
    provider: BluetoothInputProvider,
    commands: mpsc::UnboundedReceiver<BluetoothCommand>,
    snapshot: Arc<RwLock<serde_json::Value>>,
    controllers: Arc<RwLock<Vec<BluetoothControllerState>>>,
    sources: InputSourceCatalog,
    availability: [Option<bool>; 2],
    discovery_enabled: bool,
    active_scans: [bool; 2],
    session_active: bool,
    devices: BTreeMap<String, BluetoothDevice>,
    identities: BTreeMap<String, ControllerId>,
    player_assignments: HashMap<String, u8>,
    applied_players: HashMap<u64, u8>,
    error: Option<String>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct BluetoothDevice {
    id: String,
    name: String,
    connected: bool,
    pairing: bool,
}

impl BluetoothHandle {
    pub(super) fn start(router: PhysicalInputRouter, sources: InputSourceCatalog) -> Self {
        let (commands, receiver) = mpsc::unbounded_channel();
        let snapshot = Arc::new(RwLock::new(Self::unavailable_snapshot()));
        let controllers = Arc::new(RwLock::new(Vec::new()));
        let handle = Self {
            commands,
            snapshot: snapshot.clone(),
            controllers: controllers.clone(),
        };
        tokio::spawn(async move {
            match BluetoothInputProvider::start(ManagerConfig::default(), router).await {
                Ok(provider) => {
                    BluetoothActor {
                        provider,
                        commands: receiver,
                        snapshot,
                        controllers,
                        sources,
                        availability: [None, None],
                        discovery_enabled: false,
                        active_scans: [false, false],
                        session_active: false,
                        devices: BTreeMap::new(),
                        identities: BTreeMap::new(),
                        player_assignments: HashMap::new(),
                        applied_players: HashMap::new(),
                        error: None,
                    }
                    .run()
                    .await;
                }
                Err(error) => {
                    *snapshot.write() = serde_json::json!({
                        "availability": { "osBle": "unavailable", "winUsb": "unavailable" },
                        "discovery": { "enabled": false, "active": false },
                        "devices": [],
                        "error": error.to_string(),
                    });
                }
            }
        });
        handle
    }

    pub(super) fn snapshot(&self) -> serde_json::Value {
        self.snapshot.read().clone()
    }

    pub(super) fn controllers(&self) -> Vec<BluetoothControllerState> {
        self.controllers.read().clone()
    }

    pub(super) fn set_session_active(&self, active: bool) {
        let _ = self.commands.send(BluetoothCommand::SessionActive(active));
    }

    pub(super) fn assign_players(&self, assignments: Vec<(String, u8)>) {
        let _ = self
            .commands
            .send(BluetoothCommand::AssignPlayers(assignments));
    }

    pub(super) fn disconnect_sources(&self, sources: Vec<String>) {
        let _ = self
            .commands
            .send(BluetoothCommand::DisconnectSources(sources));
    }

    pub(super) fn connect(&self, id: String) {
        self.enqueue_api(serde_json::json!({ "type": "bluetooth.connect", "id": id }));
    }

    pub(super) fn forget(&self, id: String) {
        self.enqueue_api(serde_json::json!({ "type": "bluetooth.forget", "id": id }));
    }

    fn enqueue_api(&self, value: serde_json::Value) {
        let (result, _response) = oneshot::channel();
        let _ = self.commands.send(BluetoothCommand::Api { value, result });
    }

    fn unavailable_snapshot() -> serde_json::Value {
        serde_json::json!({
            "availability": { "osBle": "unknown", "winUsb": "unknown" },
            "discovery": { "enabled": false, "active": false },
            "devices": [],
        })
    }
}

impl BluetoothActor {
    async fn run(mut self) {
        let mut events = self.provider.events();
        self.publish();
        loop {
            tokio::select! {
                command = self.commands.recv() => {
                    let Some(command) = command else { break };
                    self.handle_command(command).await;
                }
                event = events.recv() => match event {
                    Ok(event) => self.handle_event(event),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            self.publish();
        }
    }

    async fn handle_command(&mut self, command: BluetoothCommand) {
        match command {
            BluetoothCommand::Api { value, result } => {
                let response = self.handle_api(value).await;
                self.publish();
                let response = response.map(|()| self.snapshot.read().clone());
                let _ = result.send(response);
            }
            BluetoothCommand::SessionActive(active) => {
                self.session_active = active;
                self.refresh_discovery().await;
            }
            BluetoothCommand::AssignPlayers(assignments) => {
                self.player_assignments = assignments.iter().cloned().collect();
                let manager = self.provider.manager();
                let controllers = self.provider.controllers();
                for (source, player) in assignments {
                    if let Some(controller) = controllers
                        .iter()
                        .find(|controller| controller.source_id.as_str() == source)
                    {
                        let _ = manager
                            .set_player_index(controller.session_id, player)
                            .await;
                        self.applied_players
                            .insert(controller.session_id.get(), player);
                    }
                }
            }
            BluetoothCommand::DisconnectSources(sources) => {
                let manager = self.provider.manager();
                let controllers = self.provider.controllers();
                for source in sources {
                    if let Some(controller) = controllers
                        .iter()
                        .find(|controller| controller.source_id.as_str() == source)
                    {
                        let _ = manager.sleep(controller.session_id).await;
                    }
                }
            }
        }
    }

    async fn handle_api(&mut self, value: serde_json::Value) -> Result<(), String> {
        let kind = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Bluetooth command requires a type".to_owned())?;
        match kind {
            "bluetooth.connect" => {
                let id = Self::device_id(&value)?;
                let controller =
                    self.identities.get(id).copied().ok_or_else(|| {
                        format!("Bluetooth device '{id}' has not been discovered")
                    })?;
                self.provider
                    .manager()
                    .connect(
                        controller.address,
                        ConnectOptions {
                            bonding_mode: value
                                .get("bondingMode")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned),
                            ..ConnectOptions::default()
                        },
                    )
                    .await
                    .map_err(|error| {
                        tracing::error!(address = %controller.address, %error, "Bluetooth connection failed");
                        error.to_string()
                    })?;
            }
            "bluetooth.disconnect" => {
                let id = Self::device_id(&value)?;
                let controller = self
                    .provider
                    .controllers()
                    .into_iter()
                    .find(|controller| controller.source_id.as_str() == id)
                    .ok_or_else(|| format!("Bluetooth device '{id}' is not connected"))?;
                self.provider
                    .manager()
                    .sleep(controller.session_id)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            "bluetooth.forget" => {
                let id = Self::device_id(&value)?;
                let controller = self
                    .identities
                    .get(id)
                    .copied()
                    .ok_or_else(|| format!("Bluetooth device '{id}' is unknown"))?;
                self.provider
                    .manager()
                    .forget(controller)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            _ => return Err(format!("unknown Bluetooth command '{kind}'")),
        }
        Ok(())
    }

    async fn refresh_discovery(&mut self) {
        let enabled = self.session_active;
        if enabled == self.discovery_enabled {
            return;
        }
        self.discovery_enabled = enabled;
        if enabled {
            if self.availability[0] == Some(true) && !self.active_scans[0] {
                self.start_scan(TransportKind::Ble);
            }
            if self.availability[1] == Some(true) && !self.active_scans[1] {
                self.start_scan(TransportKind::BrEdr);
            }
        } else {
            let manager = self.provider.manager();
            if self.active_scans[0] {
                let _ = manager.stop_scan(TransportKind::Ble).await;
            }
            if self.active_scans[1] {
                let _ = manager.stop_scan(TransportKind::BrEdr).await;
            }
        }
    }

    fn handle_event(&mut self, event: BluetoothProviderEvent) {
        match event {
            BluetoothProviderEvent::Manager(event) => self.handle_manager_event(event),
            BluetoothProviderEvent::Input(controller) => {
                self.observe_controller(*controller);
            }
            _ => {}
        }
    }

    fn handle_manager_event(&mut self, event: ManagerEvent) {
        match event {
            ManagerEvent::AdapterReady { transport, .. } => {
                self.availability[Self::transport_index(transport)] = Some(true);
                if self.discovery_enabled {
                    self.start_scan(transport);
                }
            }
            ManagerEvent::AdapterUnavailable { transport, .. } => {
                self.availability[Self::transport_index(transport)] = Some(false);
            }
            ManagerEvent::ScanState { transport, active } => {
                self.active_scans[Self::transport_index(transport)] = active;
                if !active && self.discovery_enabled {
                    self.start_scan(transport);
                }
            }
            ManagerEvent::Discovered {
                identity,
                profile_id,
                ..
            } => {
                let Some(profile_id) = profile_id else {
                    return;
                };
                let controller = ControllerId::new(identity.transport, identity.address);
                let id = source_id(controller).as_str().to_owned();
                self.identities.insert(id.clone(), controller);
                self.devices.insert(
                    id.clone(),
                    BluetoothDevice {
                        id,
                        name: identity.name.unwrap_or_else(|| profile_id.to_string()),
                        connected: false,
                        pairing: false,
                    },
                );
            }
            ManagerEvent::Connecting { controller_id, .. } => {
                self.error = None;
                self.active_scans[Self::transport_index(controller_id.transport)] = false;
                let id = source_id(controller_id).as_str().to_owned();
                if let Some(device) = self.devices.get_mut(&id) {
                    device.pairing = true;
                }
            }
            ManagerEvent::Connected(controller) => {
                self.error = None;
                let id = source_id(controller.controller_id).as_str().to_owned();
                self.identities.insert(id.clone(), controller.controller_id);
                self.devices.insert(
                    id.clone(),
                    BluetoothDevice {
                        id,
                        name: controller
                            .identity
                            .name
                            .unwrap_or_else(|| controller.profile_id.to_string()),
                        connected: true,
                        pairing: false,
                    },
                );
            }
            ManagerEvent::Disconnected { controller_id, .. } => {
                let id = source_id(controller_id).as_str().to_owned();
                if let Some(device) = self.devices.get_mut(&id) {
                    device.connected = false;
                    device.pairing = false;
                }
            }
            ManagerEvent::Error { address, message } => {
                tracing::error!(address = ?address, error = %message, "Bluetooth manager operation failed");
                self.error = Some(message);
                if let Some(address) = address
                    && let Some(id) = self.identities.iter().find_map(|(id, controller)| {
                        (controller.address == address).then(|| id.clone())
                    })
                    && let Some(device) = self.devices.get_mut(&id)
                {
                    device.pairing = false;
                }
            }
            _ => {}
        }
    }

    fn observe_controller(&mut self, controller: BluetoothControllerState) {
        let id = controller.source_id.as_str().to_owned();
        self.sources.upsert(InputSourceDescriptor {
            id: id.clone(),
            source: InputSourceKind::Bt,
            name: controller.name.clone(),
            connected: controller.connected,
            metadata: serde_json::json!({
                "profileId": controller.profile_id,
                "batteryLevel": controller.battery_level,
                "charging": controller.charging,
            }),
        });
        self.devices.insert(
            id.clone(),
            BluetoothDevice {
                id: id.clone(),
                name: controller.name.clone(),
                connected: controller.connected,
                pairing: false,
            },
        );
        if controller.connected
            && let Some(player) = self.player_assignments.get(&id).copied()
            && self.applied_players.get(&controller.session_id.get()) != Some(&player)
        {
            self.applied_players
                .insert(controller.session_id.get(), player);
            let manager = self.provider.manager();
            let session_id = controller.session_id;
            tokio::spawn(async move {
                let _ = manager.set_player_index(session_id, player).await;
            });
        }
        let mut controllers = self.controllers.write();
        if let Some(current) = controllers
            .iter_mut()
            .find(|current| current.controller_id == controller.controller_id)
        {
            *current = controller;
        } else {
            controllers.push(controller);
        }
    }

    fn publish(&self) {
        *self.snapshot.write() = serde_json::json!({
            "availability": {
                "osBle": Self::availability_name(self.availability[0]),
                "winUsb": Self::availability_name(self.availability[1]),
            },
            "discovery": {
                "enabled": self.discovery_enabled,
                "active": self.active_scans.iter().any(|active| *active),
            },
            "devices": self.devices.values().collect::<Vec<_>>(),
            "error": self.error,
        });
    }

    fn start_scan(&self, transport: TransportKind) {
        let manager = self.provider.manager();
        tokio::spawn(async move {
            let request = match transport {
                TransportKind::Ble => ScanRequest::Ble {
                    duration: Duration::from_secs(30),
                },
                TransportKind::BrEdr => ScanRequest::BrEdr { duration_units: 8 },
            };
            let _ = manager.scan(request).await;
        });
    }

    fn device_id(value: &serde_json::Value) -> Result<&str, String> {
        value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Bluetooth device command requires an id".to_owned())
    }

    const fn transport_index(transport: TransportKind) -> usize {
        match transport {
            TransportKind::Ble => 0,
            TransportKind::BrEdr => 1,
        }
    }

    const fn availability_name(value: Option<bool>) -> &'static str {
        match value {
            Some(true) => "available",
            Some(false) => "unavailable",
            None => "unknown",
        }
    }
}
