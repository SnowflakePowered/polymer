use snowglass_bluetooth::{
    ControllerId, ManagerEvent, ScanRequest, SessionId, TransportKind, devices::ConnectOptions,
};
use snowglass_input::{BackendId, DeviceId};
use snowglass_input_bluetooth::{BluetoothBackend, DiscoveryEvent};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::broadcast;

pub(super) struct BluetoothUi {
    manager: snowglass_bluetooth::BluetoothManager,
    events: broadcast::Receiver<DiscoveryEvent>,
    availability: [Option<bool>; 2],
    scans: [bool; 2],
    session_active: bool,
    devices: BTreeMap<String, Candidate>,
    identities: BTreeMap<String, ControllerId>,
    sessions: BTreeMap<String, SessionId>,
    error: Option<String>,
}

#[derive(Clone)]
struct Candidate {
    id: String,
    name: String,
    profile: Option<String>,
    vendor_id: Option<u16>,
    product_id: Option<u16>,
    pairing: bool,
}

impl BluetoothUi {
    pub(super) fn new(backend: &Arc<BluetoothBackend>) -> Self {
        Self {
            manager: backend.manager(),
            events: backend.discovery(),
            availability: [None, None],
            scans: [false, false],
            session_active: false,
            devices: BTreeMap::new(),
            identities: BTreeMap::new(),
            sessions: BTreeMap::new(),
            error: None,
        }
    }

    pub(super) fn set_session_active(&mut self, active: bool) {
        self.session_active = active;
        for transport in [TransportKind::Ble, TransportKind::BrEdr] {
            let index = Self::transport_index(transport);
            if active && self.availability[index] != Some(false) && !self.scans[index] {
                self.start_scan(transport);
            } else if !active && self.scans[index] {
                let manager = self.manager.clone();
                tokio::spawn(async move {
                    let _ = manager.stop_scan(transport).await;
                });
            }
        }
    }

    pub(super) fn connect(&mut self, id: &str) -> Result<(), String> {
        let controller = self
            .identities
            .get(id)
            .copied()
            .ok_or_else(|| format!("Bluetooth device '{id}' has not been discovered"))?;
        if let Some(device) = self.devices.get_mut(id) {
            device.pairing = true;
        }
        self.error = None;
        let manager = self.manager.clone();
        tokio::spawn(async move {
            if let Err(error) = manager
                .connect(controller.address, ConnectOptions::default())
                .await
            {
                tracing::error!(address = %controller.address, %error, "Bluetooth connection failed");
            }
        });
        Ok(())
    }

    pub(super) fn forget(&mut self, id: &str) -> Result<(), String> {
        let controller = self
            .identities
            .get(id)
            .copied()
            .ok_or_else(|| format!("Bluetooth device '{id}' is unknown"))?;
        self.devices.remove(id);
        self.identities.remove(id);
        self.sessions.remove(id);
        let manager = self.manager.clone();
        tokio::spawn(async move {
            let _ = manager.forget(controller).await;
        });
        Ok(())
    }

    pub(super) fn disconnect(&mut self, id: &str) {
        let Some(session) = self.sessions.get(id).copied() else {
            return;
        };
        let manager = self.manager.clone();
        tokio::spawn(async move {
            let _ = manager.sleep(session).await;
        });
    }

    pub(super) fn merge(&mut self, controllers: &mut serde_json::Value) {
        self.drain();
        let bluetooth_available = if self.availability.contains(&Some(true)) {
            Some(true)
        } else if self.availability.iter().all(|value| *value == Some(false)) {
            Some(false)
        } else {
            None
        };
        controllers["availability"] = serde_json::json!({
            "osBle": Self::availability_name(bluetooth_available),
            "winUsb": "unknown",
        });
        let connected = controllers["devices"]
            .as_array()
            .map(|devices| {
                devices
                    .iter()
                    .filter(|device| device["connected"].as_bool() == Some(true))
                    .filter_map(|device| device["id"].as_str().map(str::to_owned))
                    .collect::<std::collections::HashSet<_>>()
            })
            .unwrap_or_default();
        let candidates = self
            .devices
            .values()
            .filter(|device| !connected.contains(&device.id))
            .map(Candidate::value)
            .collect::<Vec<_>>();
        if let Some(devices) = controllers["devices"].as_array_mut() {
            for candidate in &candidates {
                if let Some(device) = devices
                    .iter_mut()
                    .find(|device| device["id"] == candidate["id"])
                {
                    device["pairing"] = candidate["pairing"].clone();
                } else {
                    devices.push(candidate.clone());
                }
            }
        }
        if let Some(session) = controllers["activeConnectionSession"].as_object_mut() {
            session.insert(
                "discovery".to_owned(),
                serde_json::json!({ "active": self.scans.iter().any(|active| *active) }),
            );
            session.insert(
                "candidates".to_owned(),
                serde_json::Value::Array(candidates),
            );
            if let Some(error) = &self.error {
                session.insert("error".to_owned(), error.clone().into());
            }
        }
    }

    fn drain(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(DiscoveryEvent::Manager(event)) => self.handle(event),
                Ok(DiscoveryEvent::UnsupportedReportFormat { .. })
                | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(
                    broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed,
                ) => {
                    break;
                }
            }
        }
    }

    fn handle(&mut self, event: ManagerEvent) {
        match event {
            ManagerEvent::AdapterReady { transport, .. } => {
                self.availability[Self::transport_index(transport)] = Some(true);
                if self.session_active {
                    self.start_scan(transport);
                }
            }
            ManagerEvent::AdapterUnavailable { transport, .. } => {
                self.availability[Self::transport_index(transport)] = Some(false);
            }
            ManagerEvent::ScanState { transport, active } => {
                self.scans[Self::transport_index(transport)] = active;
                if !active && self.session_active {
                    self.start_scan(transport);
                }
            }
            ManagerEvent::Discovered {
                identity,
                profile_id: Some(profile),
                ..
            } => {
                let controller = ControllerId::new(identity.transport, identity.address);
                let id = Self::device_id(controller);
                let vendor_id = identity.pnp.as_ref().map(|value| value.vendor_id);
                let product_id = identity.pnp.as_ref().map(|value| value.product_id);
                self.identities.insert(id.clone(), controller);
                self.devices.insert(
                    id.clone(),
                    Candidate {
                        id,
                        name: identity.name.unwrap_or_else(|| profile.to_string()),
                        profile: Some(profile.to_string()),
                        vendor_id,
                        product_id,
                        pairing: false,
                    },
                );
            }
            ManagerEvent::Connecting { controller_id, .. } => {
                self.scans[Self::transport_index(controller_id.transport)] = false;
                if let Some(device) = self.devices.get_mut(&Self::device_id(controller_id)) {
                    device.pairing = true;
                }
            }
            ManagerEvent::Connected(controller) => {
                let id = Self::device_id(controller.controller_id);
                let vendor_id = controller
                    .identity
                    .pnp
                    .as_ref()
                    .map(|value| value.vendor_id);
                let product_id = controller
                    .identity
                    .pnp
                    .as_ref()
                    .map(|value| value.product_id);
                self.identities.insert(id.clone(), controller.controller_id);
                self.sessions.insert(id.clone(), controller.session_id);
                self.devices.insert(
                    id.clone(),
                    Candidate {
                        id,
                        name: controller
                            .identity
                            .name
                            .unwrap_or_else(|| controller.profile_id.to_string()),
                        profile: Some(controller.profile_id.to_string()),
                        vendor_id,
                        product_id,
                        pairing: false,
                    },
                );
            }
            ManagerEvent::Disconnected { controller_id, .. } => {
                if let Some(device) = self.devices.get_mut(&Self::device_id(controller_id)) {
                    device.pairing = false;
                }
            }
            ManagerEvent::Error { address, message } => {
                self.error = Some(message);
                if let Some(address) = address {
                    let id = self.identities.iter().find_map(|(id, controller)| {
                        (controller.address == address).then(|| id.clone())
                    });
                    if let Some(device) = id.and_then(|id| self.devices.get_mut(&id)) {
                        device.pairing = false;
                    }
                }
            }
            _ => {}
        }
    }

    fn start_scan(&self, transport: TransportKind) {
        let manager = self.manager.clone();
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

    fn device_id(controller: ControllerId) -> String {
        let transport = match controller.transport {
            TransportKind::Ble => "ble",
            TransportKind::BrEdr => "br-edr",
        };
        DeviceId::new(
            BackendId::BLUETOOTH,
            &format!("{transport}/{}", controller.address),
        )
        .to_string()
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

impl Candidate {
    fn value(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "profile": self.profile,
            "vendorId": self.vendor_id,
            "productId": self.product_id,
            "serial": null,
            "role": "Whole",
            "capabilities": [],
            "index": null,
            "connected": false,
            "battery": null,
            "pairing": self.pairing,
        })
    }
}
