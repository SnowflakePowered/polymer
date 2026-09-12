//! DS4 USB provisioning: feature 0x12 reads addresses; 0x13 writes host + key.
//! References: ds4-tools/ds4-tool.py and dsremap's USB protocol captures.
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::{
    CablePairingHost, CablePairingState, VerifiedCablePairing, feature::FeaturePairing,
    hid::UsbFeatureReports,
};
use crate::{DeviceAddr, Error, Result, transport::bredr::LinkKey};

pub(super) struct DualShock4Pairing<D> {
    pub(super) device: D,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct PairingReport {
    id: u8,
    controller: [u8; 6],
    reserved: [u8; 3],
    host: [u8; 6],
}

const _: () = assert!(std::mem::size_of::<PairingReport>() == 16);

impl<D: UsbFeatureReports> FeaturePairing for DualShock4Pairing<D> {
    fn read_pairing(&mut self) -> Result<CablePairingState> {
        let mut bytes = [0; 16];
        bytes[0] = 0x12;
        let length = self.device.get_feature(&mut bytes)?;
        let report = PairingReport::ref_from_bytes(
            bytes
                .get(..length)
                .ok_or(Error::InvalidPacket("oversized DS4 pairing report"))?,
        )
        .map_err(|_| Error::InvalidPacket("short DS4 pairing report"))?;
        if report.id != 0x12
            || (report.controller == [0; 6] || report.controller == [255; 6])
            || report.host == [255; 6]
        {
            return Err(Error::InvalidPacket("invalid DS4 pairing report"));
        }
        Ok(CablePairingState {
            controller_address: DeviceAddr::from_hci_bytes(report.controller),
            host_address: (report.host != [0; 6]).then(|| DeviceAddr::from_hci_bytes(report.host)),
        })
    }

    fn pair(&mut self, host: &CablePairingHost) -> Result<VerifiedCablePairing> {
        host.validate()?;
        if host.adapter_index.is_some() {
            return Err(Error::Unsupported(
                "DS4 cable pairing requires the dedicated BR/EDR adapter",
            ));
        }
        let before = self.read_pairing()?;
        // Address readback does not expose the key. Even if the host matches,
        // provision a fresh key so recovery from a lost registry is possible.
        let key = rand::random::<[u8; 16]>();
        let mut report = [0; 23];
        report[0] = 0x13;
        report[1..7].copy_from_slice(&host.address.hci_bytes());
        report[7..].copy_from_slice(&key);
        self.device.set_feature(&report)?;
        let after = self.read_pairing()?;
        if after.controller_address != before.controller_address
            || after.host_address != Some(host.address)
        {
            return Err(Error::InvalidPacket(
                "DS4 cable pairing readback did not match; retry cable pairing",
            ));
        }
        // USB confirms the address, not the secret. Authentication on the next
        // wireless connection is the final check that both sides share this key.
        Ok(VerifiedCablePairing {
            state: after,
            link_key: Some(LinkKey {
                value: key,
                key_type: 0x00,
            }), // Combination key provisioned over USB.
        })
    }
}

#[cfg(test)]
mod tests;
