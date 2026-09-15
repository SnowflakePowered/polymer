use super::{
    CablePairingHost, CablePairingState, VerifiedCablePairing, feature::FeaturePairing,
    hid::UsbFeatureReports,
};
use crate::{DeviceAddr, Error, Result};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

pub(super) struct DualShock3Pairing<D> {
    pub(super) device: D,
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

impl<D: UsbFeatureReports> FeaturePairing for DualShock3Pairing<D> {
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
