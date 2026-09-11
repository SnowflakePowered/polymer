use super::{
    CableDeviceIdentity, CablePairingHost, CablePairingSession, CablePairingState,
    VerifiedCablePairing,
};
use crate::{DeviceAddr, Error, Result, devices::ProfileId};
use async_trait::async_trait;
use polymer_dshidmini::{Device, PairingState};

pub(super) struct Session {
    identity: CableDeviceIdentity,
    device: Device,
}

impl Session {
    pub fn new(identity: CableDeviceIdentity, device: Device) -> Self {
        Self { identity, device }
    }

    pub async fn enumerate() -> Result<Vec<CableDeviceIdentity>> {
        tokio::task::spawn_blocking(|| {
            Device::enumerate()
                .map_err(Self::error)?
                .into_iter()
                .map(|device| {
                    Ok(CableDeviceIdentity {
                        profile_id: ProfileId::new("sony.dualshock3"),
                        vendor_id: 0x054c,
                        product_id: 0x0268,
                        name: "DualShock 3 / SIXAXIS (DsHidMini)".into(),
                        serial: Some(DeviceAddr::public(device.address).to_string()),
                        path: std::ffi::CString::new(device.instance_id.as_bytes())
                            .map_err(|_| Error::InvalidPacket("invalid DsHidMini instance ID"))?,
                        usb: None,
                        dshidmini: Some(device),
                    })
                })
                .collect()
        })
        .await
        .map_err(|error| Error::Platform(format!("DsHidMini enumeration task: {error}")))?
    }

    fn state(state: PairingState) -> CablePairingState {
        CablePairingState {
            controller_address: DeviceAddr::public(state.controller_address),
            host_address: state.host_address.map(DeviceAddr::public),
        }
    }

    fn error(error: std::io::Error) -> Error {
        Error::Platform(format!("DsHidMini pairing: {error}"))
    }
}

#[async_trait]
impl CablePairingSession for Session {
    fn identity(&self) -> &CableDeviceIdentity {
        &self.identity
    }

    async fn read_pairing(&mut self) -> Result<CablePairingState> {
        let device = self.device.clone();
        tokio::task::spawn_blocking(move || {
            device.read_pairing().map(Self::state).map_err(Self::error)
        })
        .await
        .map_err(|error| Error::Platform(format!("DsHidMini read task: {error}")))?
    }

    async fn pair(&mut self, host: &CablePairingHost) -> Result<VerifiedCablePairing> {
        host.validate()?;
        let device = self.device.clone();
        let host = host.address.bytes();
        let state = tokio::task::spawn_blocking(move || {
            device.pair(host).map(Self::state).map_err(Self::error)
        })
        .await
        .map_err(|error| Error::Platform(format!("DsHidMini pairing task: {error}")))??;
        Ok(VerifiedCablePairing {
            state,
            link_key: None,
        })
    }
}
