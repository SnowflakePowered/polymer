//! USB provisioning of persistent Bluetooth pairings. USB input remains owned
//! by SDL outside provisioning; these sessions only perform the controller's pairing handshake.

use std::{ffi::CString, sync::Arc};

use async_trait::async_trait;

use crate::{
    AddressKind, DeviceAddr, Error, Result, TransportKind,
    devices::{ProfileId, VendorProductId},
    store::{ControllerRecord, ControllerStore},
    transport::bredr::LinkKey,
};

#[cfg(windows)]
mod dshidmini;
mod dualshock3;
mod feature;
mod hid;
mod sony;
mod switch;
mod switch2;
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
    #[cfg(windows)]
    dshidmini: Option<polymer_dshidmini::Device>,
}

impl CableDeviceIdentity {
    pub fn path(&self) -> &std::ffi::CStr {
        &self.path
    }

    pub async fn open(&self) -> Result<Box<dyn CablePairingSession>> {
        #[cfg(windows)]
        if let Some(device) = &self.dshidmini {
            return Ok(Box::new(dshidmini::Session::new(
                self.clone(),
                device.clone(),
            )));
        }
        let identity = self.clone();
        match identity.profile_id.as_str() {
            "sony.dualshock3" => {
                if let Some(info) = &identity.usb {
                    let device = usb::DualShock3Usb::open(info).await?;
                    Ok(Box::new(feature::FeatureSession::new(
                        identity,
                        dualshock3::DualShock3Pairing { device },
                    )))
                } else {
                    let device = hid::CableHid::open(identity.clone()).await?;
                    Ok(Box::new(feature::FeatureSession::new(
                        identity,
                        dualshock3::DualShock3Pairing { device },
                    )))
                }
            }
            "sony.dualshock4" | "sony.dualsense" => {
                let model = if identity.profile_id.as_str() == "sony.dualsense" {
                    sony::Model::DualSense
                } else {
                    sony::Model::DualShock4
                };
                let device = hid::CableHid::open(identity.clone()).await?;
                Ok(Box::new(feature::FeatureSession::new(
                    identity,
                    sony::SonyPairing { device, model },
                )))
            }
            "nintendo.switch.pro" => {
                let device = hid::CableHid::open(identity.clone()).await?;
                Ok(Box::new(feature::FeatureSession::new(
                    identity,
                    switch::Pairing::new(device),
                )))
            }
            id if id.starts_with("nintendo.switch2.") => {
                let info = identity
                    .usb
                    .as_ref()
                    .ok_or(Error::Unsupported("Switch 2 USB command interface"))?;
                let commands = switch2::UsbCommands::open(info).await?;
                Ok(Box::new(switch2::Session::new(identity, commands)))
            }
            _ => Err(Error::Unsupported("USB cable pairing for this controller")),
        }
    }

    /// Wireless transport provisioned by this controller's USB protocol.
    pub fn wireless_transport(&self) -> TransportKind {
        if self.profile_id.as_str().starts_with("nintendo.switch2.") {
            TransportKind::Ble
        } else {
            TransportKind::BrEdr
        }
    }

    pub fn access_label(&self) -> &'static str {
        if self.profile_id.as_str() == "nintendo.switch.pro" {
            return "USB HID commands";
        }
        if self.wireless_transport() == TransportKind::Ble {
            return "USB vendor commands (interface 1)";
        }
        if self.uses_dshidmini() {
            return "DsHidMini IPC";
        }
        if self.usb.is_some() {
            "WinUSB control transfers"
        } else {
            "OS HID feature reports"
        }
    }

    pub fn uses_winusb(&self) -> bool {
        self.usb.is_some()
    }

    pub fn uses_dshidmini(&self) -> bool {
        #[cfg(windows)]
        {
            self.dshidmini.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CablePairingState {
    pub controller_address: DeviceAddr,
    pub host_address: Option<DeviceAddr>,
}

#[derive(Clone, Copy, Debug)]
pub struct CablePairingHost {
    /// Address of the radio used for wireless reconnect: the dedicated adapter
    /// for BR/EDR controllers, or the OS-owned BLE adapter for Switch 2.
    pub address: DeviceAddr,
    /// OS BLE adapter index. `None` selects the dedicated BR/EDR radio.
    pub adapter_index: Option<usize>,
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
        devices.extend(switch2::UsbCommands::enumerate().await?);
        #[cfg(windows)]
        devices.extend(dshidmini::Session::enumerate().await?);
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
        let transport = session.identity().wireless_transport();
        if (transport == TransportKind::Ble) != host.adapter_index.is_some() {
            return Err(Error::Unsupported(
                "cable pairing host uses the wrong wireless transport",
            ));
        }
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
            .find(|record| record.address == address && record.transport == transport)
            .map_or(0, |record| record.player_index);
        let record = ControllerRecord {
            address,
            transport,
            profile_id: session.identity().profile_id.clone(),
            vendor_product: Some(VendorProductId::new(
                session.identity().vendor_id,
                session.identity().product_id,
            )),
            // Do not carry stale keys from a previous pairing into a cable bond.
            link_key: verified.link_key.as_ref().map(|key| key.value),
            link_key_type: verified.link_key.as_ref().map(|key| key.key_type),
            adapter_index: host.adapter_index,
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
