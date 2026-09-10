use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::{
    CableDeviceIdentity, CablePairingHost, CablePairingSession, CablePairingState,
    VerifiedCablePairing, hid::UsbFeatureReports,
};
use crate::{DeviceAddr, Error, Result};

pub(super) struct DualShock3Session<D> {
    identity: CableDeviceIdentity,
    protocol: Arc<Mutex<DualShock3Pairing<D>>>,
}

impl<D> DualShock3Session<D> {
    pub fn new(identity: CableDeviceIdentity, device: D) -> Self {
        Self {
            identity,
            protocol: Arc::new(Mutex::new(DualShock3Pairing { device })),
        }
    }
}

#[async_trait]
impl<D: UsbFeatureReports + Send + 'static> CablePairingSession for DualShock3Session<D> {
    fn identity(&self) -> &CableDeviceIdentity {
        &self.identity
    }

    async fn read_pairing(&mut self) -> Result<CablePairingState> {
        let protocol = self.protocol.clone();
        tokio::task::spawn_blocking(move || {
            protocol
                .lock()
                .map_err(|_| Error::ChannelClosed)?
                .read_pairing()
        })
        .await
        .map_err(|error| Error::Platform(format!("USB pairing read task failed: {error}")))?
    }

    async fn pair(&mut self, host: &CablePairingHost) -> Result<VerifiedCablePairing> {
        host.validate()?;
        let host = *host;
        let protocol = self.protocol.clone();
        tokio::task::spawn_blocking(move || {
            protocol
                .lock()
                .map_err(|_| Error::ChannelClosed)?
                .pair(&host)
        })
        .await
        .map_err(|error| Error::Platform(format!("USB pairing task failed: {error}")))?
    }
}

struct DualShock3Pairing<D> {
    device: D,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct DeviceAddressReport {
    reserved: [u8; 4],
    address: [u8; 6],
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct HostAddressReport {
    reserved: [u8; 2],
    address: [u8; 6],
}

impl<D: UsbFeatureReports> DualShock3Pairing<D> {
    fn read_pairing(&mut self) -> Result<CablePairingState> {
        // Report sizes and offsets follow BlueZ plugins/sixaxis.c. Addresses
        // here are display/network order, unlike the little-endian HCI wire.
        let mut device = [0; 18];
        device[0] = 0xf2;
        let length = self.device.get_feature(&mut device)?;
        let (device, _) = DeviceAddressReport::ref_from_prefix(
            device
                .get(..length)
                .ok_or(Error::InvalidPacket("oversized DS3 F2 feature report"))?,
        )
        .map_err(|_| Error::InvalidPacket("short DS3 F2 feature report"))?;
        if device.address == [0; 6] || device.address == [255; 6] {
            return Err(Error::InvalidPacket(
                "invalid DS3 controller address report",
            ));
        }
        let controller_address = DeviceAddr::public(device.address);
        let mut host = [0; 8];
        host[0] = 0xf5;
        let length = self.device.get_feature(&mut host)?;
        let host = HostAddressReport::ref_from_bytes(
            host.get(..length)
                .ok_or(Error::InvalidPacket("oversized DS3 F5 feature report"))?,
        )
        .map_err(|_| Error::InvalidPacket("short DS3 F5 feature report"))?;
        Ok(CablePairingState {
            controller_address,
            host_address: (host.address != [0; 6]).then_some(DeviceAddr::public(host.address)),
        })
    }

    fn pair(&mut self, host: &CablePairingHost) -> Result<VerifiedCablePairing> {
        host.validate()?;
        let before = self.read_pairing()?;
        if before.host_address != Some(host.address) {
            let mut report = [0xf5, 0x01, 0, 0, 0, 0, 0, 0];
            report[2..].copy_from_slice(&host.address.bytes());
            self.device.set_feature(&report)?;
        }
        let after = self.read_pairing()?;
        if after.controller_address != before.controller_address
            || after.host_address != Some(host.address)
        {
            return Err(Error::InvalidPacket(
                "DS3 cable pairing readback did not match",
            ));
        }
        // No runtime stream or teardown command: F5 remains stored in the pad
        // after this USB handle closes and the cable is unplugged.
        Ok(VerifiedCablePairing {
            state: after,
            link_key: None,
        })
    }
}

#[cfg(test)]
mod tests;
