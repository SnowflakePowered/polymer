use std::ffi::CString;

use dashmap::DashSet;
use futures::StreamExt;
use nusb::hotplug::HotplugEvent;

use super::*;
use crate::cable_pairing::{CableDeviceIdentity, CablePairing, CablePairingHost};

const CABLE_ENUMERATION_DELAY: Duration = Duration::from_millis(750);
const CABLE_RELEASE_DELAY: Duration = Duration::from_millis(250);

struct CableProvisioner {
    coordinator: CablePairing,
    store: Arc<dyn ControllerStore>,
    held: Arc<DashSet<String>>,
    internal: Internal,
    bredr_host: Option<CablePairingHost>,
    ble_host: Option<CablePairingHost>,
    provisioned: HashSet<CString>,
}

struct HeldDevice {
    held: Arc<DashSet<String>>,
    path: String,
}

impl HeldDevice {
    fn hold(held: &Arc<DashSet<String>>, device: &CableDeviceIdentity) -> Self {
        let path = device.path().to_string_lossy().to_lowercase();
        held.insert(path.clone());
        Self {
            held: held.clone(),
            path,
        }
    }
}

impl Drop for HeldDevice {
    fn drop(&mut self) {
        self.held.remove(&self.path);
    }
}

impl BluetoothActor {
    pub(super) fn spawn_cable_pairing(
        config: &ManagerConfig,
        bredr: Option<&Arc<HciHost>>,
        internal: Internal,
    ) -> JoinHandle<()> {
        let bredr_host = bredr.map(|host| CablePairingHost {
            address: host.info().address,
            adapter_index: None,
        });
        let adapter_index = config.ble_adapter_index;
        let store = config.store.clone();
        let held = config.held_devices.clone();
        tokio::spawn(async move {
            let ble_host = default_adapter_address()
                .await
                .ok()
                .map(|address| CablePairingHost {
                    address,
                    adapter_index: Some(adapter_index),
                });
            let mut provisioner = CableProvisioner {
                coordinator: CablePairing::new(store.clone()),
                store,
                held,
                internal,
                bredr_host,
                ble_host,
                provisioned: HashSet::new(),
            };
            let mut hotplug = nusb::watch_devices()
                .inspect_err(|error| {
                    tracing::warn!(%error, "USB hotplug is unavailable; cable pairing only runs at startup");
                })
                .ok();
            loop {
                provisioner.synchronize().await;
                let Some(watch) = hotplug.as_mut() else {
                    return;
                };
                match watch.next().await {
                    Some(HotplugEvent::Connected(_)) => time::sleep(CABLE_ENUMERATION_DELAY).await,
                    Some(HotplugEvent::Disconnected(_)) => {}
                    None => return,
                }
            }
        })
    }
}

impl CableProvisioner {
    async fn synchronize(&mut self) {
        let devices = CablePairing::enumerate().await.unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to enumerate cable pairing devices");
            Vec::new()
        });
        self.provisioned.retain(|path| {
            devices
                .iter()
                .any(|device| device.path() == path.as_c_str())
        });
        for device in devices {
            if !self.provisioned.insert(device.path().to_owned()) {
                continue;
            }
            let host = match device.wireless_transport() {
                TransportKind::BrEdr => self.bredr_host,
                TransportKind::Ble => self.ble_host,
            };
            if let Some(host) = host {
                let _held = HeldDevice::hold(&self.held, &device);
                time::sleep(CABLE_RELEASE_DELAY).await;
                self.provision_attached(&device, &host).await;
            }
        }
    }

    async fn provision_attached(&self, device: &CableDeviceIdentity, host: &CablePairingHost) {
        match self.provision(device, host).await {
            Ok(Some(record)) => {
                tracing::info!(address = %record.address, profile = %record.profile_id, "cable paired controller");
                if let Some(actor) = self.internal.upgrade() {
                    actor.do_send(CablePaired(record, device.name.clone()));
                }
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(name = %device.name, %error, "cable pairing failed"),
        }
    }

    async fn provision(
        &self,
        device: &CableDeviceIdentity,
        host: &CablePairingHost,
    ) -> Result<Option<ControllerRecord>> {
        let mut session = device.open().await?;
        let verified = self.coordinator.pair(session.as_mut(), host).await?;
        let transport = device.wireless_transport();
        Ok(self.store.records()?.into_iter().find(|record| {
            record.address == verified.state.controller_address && record.transport == transport
        }))
    }
}

impl Handler<CablePaired> for BluetoothActor {
    type Result = ();

    fn handle(&mut self, CablePaired(record, name): CablePaired, _: &mut Context<Self>) {
        let _ = self.events.send(ManagerEvent::CablePaired {
            controller_id: ControllerId::new(record.transport, record.address),
            profile_id: record.profile_id.clone(),
            vendor_product: record.vendor_product,
            name,
        });
        if record.transport != TransportKind::Ble {
            return;
        }
        if let Some(task) = self.wake_tasks.remove(&record.address) {
            task.abort();
        }
        Self::arm_ble_wake(
            record,
            self.config.controllers.clone(),
            self.internal.clone(),
            self.wake_gate.clone(),
            &mut self.wake_tasks,
        );
    }
}
