//! USB provisioning of persistent Bluetooth pairings. USB input remains owned
//! by SDL; these sessions only perform the controller's pairing handshake.

use std::{ffi::CString, sync::Arc};

use async_trait::async_trait;

use crate::{
    AddressKind, DeviceAddr, Error, Result, TransportKind,
    devices::ProfileId,
    store::{ControllerRecord, ControllerStore},
    transport::bredr::LinkKey,
};

mod dualshock3;
mod hid;
mod usb;

/// A particular USB interface, rather than the first device with a matching PID.
#[derive(Clone, Debug)]
pub struct CableDeviceIdentity {
    pub profile_id: ProfileId,
    pub vendor_id: u16,
    pub product_id: u16,
    pub name: String,
    pub serial: Option<String>,
    path: CString,
    usb: Option<nusb::DeviceInfo>,
}

impl CableDeviceIdentity {
    pub fn path(&self) -> &std::ffi::CStr {
        &self.path
    }

    pub async fn open(&self) -> Result<Box<dyn CablePairingSession>> {
        let identity = self.clone();
        match identity.profile_id.as_str() {
            "sony.dualshock3" => {
                if let Some(info) = &identity.usb {
                    let device = usb::DualShock3Usb::open(info).await?;
                    Ok(Box::new(dualshock3::DualShock3Session::new(
                        identity, device,
                    )))
                } else {
                    let device = hid::CableHid::open(identity.clone()).await?;
                    Ok(Box::new(dualshock3::DualShock3Session::new(
                        identity, device,
                    )))
                }
            }
            _ => Err(Error::Unsupported("USB cable pairing for this controller")),
        }
    }

    pub fn access_label(&self) -> &'static str {
        if self.usb.is_some() {
            "WinUSB control transfers"
        } else {
            "OS HID feature reports"
        }
    }

    pub fn uses_winusb(&self) -> bool {
        self.usb.is_some()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CablePairingState {
    pub controller_address: DeviceAddr,
    pub host_address: Option<DeviceAddr>,
}

#[derive(Clone, Copy, Debug)]
pub struct CablePairingHost {
    /// The dedicated BR/EDR adapter, not the OS-owned BLE adapter.
    pub address: DeviceAddr,
}

impl CablePairingHost {
    pub fn validate(&self) -> Result<()> {
        if self.address.kind() != AddressKind::Public
            || self.address.bytes() == [0; 6]
            || self.address.bytes() == [255; 6]
        {
            return Err(Error::InvalidAddress(self.address.to_string()));
        }
        Ok(())
    }
}

/// Returned only after the device-specific handshake and verification succeed.
/// Some controllers provision a link key; DS3 provisions only the host address.
pub struct VerifiedCablePairing {
    pub state: CablePairingState,
    pub link_key: Option<LinkKey>,
}

#[async_trait]
pub trait CablePairingSession: Send {
    fn identity(&self) -> &CableDeviceIdentity;
    async fn read_pairing(&mut self) -> Result<CablePairingState>;
    async fn pair(&mut self, host: &CablePairingHost) -> Result<VerifiedCablePairing>;
}

/// Coordinates the USB handshake and durable wireless registration.
pub struct CablePairing {
    store: Arc<dyn ControllerStore>,
}

impl CablePairing {
    pub fn new(store: Arc<dyn ControllerStore>) -> Self {
        Self { store }
    }

    /// Enumerates supported cable-pairing protocols, without opening input streams.
    pub async fn enumerate() -> Result<Vec<CableDeviceIdentity>> {
        let mut devices = hid::CableHid::enumerate().await?;
        devices.extend(usb::DualShock3Usb::enumerate().await?);
        Ok(devices)
    }

    /// Registration is deliberately after readback. A store failure reports that
    /// the controller itself has already been paired; repeating this is safe.
    pub async fn pair(
        &self,
        session: &mut dyn CablePairingSession,
        host: &CablePairingHost,
    ) -> Result<VerifiedCablePairing> {
        host.validate()?;
        // Surface an unreadable registry before changing controller hardware.
        let previous = self.store.records()?;
        let verified = session.pair(host).await?;
        if verified.state.host_address != Some(host.address) {
            return Err(Error::InvalidPacket(
                "USB pairing host readback did not match",
            ));
        }
        let address = verified.state.controller_address;
        let player_index = previous
            .iter()
            .find(|record| record.address == address && record.transport == TransportKind::BrEdr)
            .map_or(0, |record| record.player_index);
        let record = ControllerRecord {
            address,
            transport: TransportKind::BrEdr,
            profile_id: session.identity().profile_id.clone(),
            // Do not carry stale keys from a previous pairing into a cable bond.
            link_key: verified.link_key.as_ref().map(|key| key.value),
            link_key_type: verified.link_key.as_ref().map(|key| key.key_type),
            adapter_index: None,
            auto_reconnect: true,
            player_index,
        };
        self.store.put(record).map_err(|error| Error::Store(format!(
            "controller {address} is paired to {}, but wireless registration failed: {error}; retry cable pairing",
            host.address,
        )))?;
        Ok(verified)
    }
}

#[cfg(test)]
mod tests;
