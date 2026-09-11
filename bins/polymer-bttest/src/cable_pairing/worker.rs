use std::sync::Arc;

use polymer_bluetooth::{
    Error, Result, TransportKind,
    cable_pairing::{CablePairing, CablePairingHost},
    store::ControllerStore,
};
use tokio::sync::mpsc;

use super::{Action, Outcome};

pub struct CablePairingWorker {
    host: Option<CablePairingHost>,
    ble_host: Option<CablePairingHost>,
    coordinator: Arc<CablePairing>,
    completions: mpsc::UnboundedSender<Result<Outcome>>,
    receiver: mpsc::UnboundedReceiver<Result<Outcome>>,
    busy: bool,
    registry_error: Option<String>,
}

impl CablePairingWorker {
    pub fn new(
        host: Option<CablePairingHost>,
        ble_host: Option<CablePairingHost>,
        store: Arc<dyn ControllerStore>,
        registry_error: Option<String>,
    ) -> Self {
        let (completions, receiver) = mpsc::unbounded_channel();
        Self {
            host,
            ble_host,
            coordinator: Arc::new(CablePairing::new(store)),
            completions,
            receiver,
            busy: false,
            registry_error,
        }
    }

    pub fn start(&mut self, action: Action) {
        if self.busy {
            return;
        }
        self.busy = true;
        let host = self.host;
        let ble_host = self.ble_host;
        let coordinator = self.coordinator.clone();
        let completions = self.completions.clone();
        let registry_error = self.registry_error.clone();
        tokio::spawn(async move {
            let result = Self::execute(action, host, ble_host, &coordinator, registry_error).await;
            let _ = completions.send(result);
        });
    }

    pub async fn receive(&mut self) -> Option<Result<Outcome>> {
        let completion = self.receiver.recv().await;
        self.busy = false;
        completion
    }

    async fn execute(
        action: Action,
        host: Option<CablePairingHost>,
        ble_host: Option<CablePairingHost>,
        coordinator: &CablePairing,
        registry_error: Option<String>,
    ) -> Result<Outcome> {
        match action {
            Action::SwitchDriver(device) => Ok(Outcome::DriverSwitched(
                super::driver::DriverSwitch::switch(device).await?,
            )),
            Action::Enumerate => Ok(Outcome::Devices(CablePairing::enumerate().await?)),
            Action::Inspect(device) => {
                let mut session = device.open().await?;
                let state = session.read_pairing().await?;
                Ok(Outcome::Inspected { device, state })
            }
            Action::Pair { device, expected } => {
                if let Some(error) = registry_error {
                    return Err(Error::Store(format!(
                        "persistent registry unavailable: {error}"
                    )));
                }
                let host = if device.wireless_transport() == TransportKind::Ble {
                    ble_host
                } else {
                    host
                }
                .ok_or(Error::Unsupported(
                    "the controller’s wireless adapter is unavailable",
                ))?;
                let mut session = device.open().await?;
                if session.read_pairing().await? != expected {
                    return Err(Error::InvalidPacket(
                        "USB controller or saved host changed; read addresses again",
                    ));
                }
                let verified = coordinator.pair(session.as_mut(), &host).await?;
                Ok(Outcome::Paired {
                    device,
                    state: verified.state,
                })
            }
        }
    }
}
