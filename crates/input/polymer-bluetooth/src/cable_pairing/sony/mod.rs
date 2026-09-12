//! Sony USB host/key provisioning. DS4 uses 0x12/0x13; DualSense uses 0x09/0x0a.
//! References: ds4-tools/ds4-tool.py and dsremap's USB protocol captures.
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::{
    CablePairingHost, CablePairingState, VerifiedCablePairing, feature::FeaturePairing,
    hid::UsbFeatureReports,
};
use crate::{DeviceAddr, Error, Result, transport::bredr::LinkKey};

pub(super) struct SonyPairing<D> {
    pub(super) device: D,
    pub(super) model: Model,
}

#[derive(Clone, Copy)]
pub(super) enum Model {
    DualShock4,
    DualSense,
}

impl Model {
    fn reports(self) -> (u8, usize, u8, usize) {
        match self {
            Self::DualShock4 => (0x12, 16, 0x13, 23),
            // DualSense has four trailing USB padding bytes on both reports.
            Self::DualSense => (0x09, 20, 0x0a, 27),
        }
    }
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

impl<D: UsbFeatureReports> FeaturePairing for SonyPairing<D> {
    fn read_pairing(&mut self) -> Result<CablePairingState> {
        let (id, size, _, _) = self.model.reports();
        let mut bytes = [0; 20];
        bytes[0] = id;
        let length = self.device.get_feature(&mut bytes[..size])?;
        if length != size {
            return Err(Error::InvalidPacket(
                "incorrect Sony USB pairing report length",
            ));
        }
        let report = PairingReport::ref_from_bytes(&bytes[..16])
            .map_err(|_| Error::InvalidPacket("invalid Sony USB pairing report"))?;
        if report.id != id
            || (report.controller == [0; 6] || report.controller == [255; 6])
            || report.host == [255; 6]
        {
            return Err(Error::InvalidPacket("invalid Sony pairing report"));
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
                "Sony cable pairing requires the dedicated BR/EDR adapter",
            ));
        }
        let before = self.read_pairing()?;
        // Address readback does not expose the key. Even if the host matches,
        // provision a fresh key so recovery from a lost registry is possible.
        let key = rand::random::<[u8; 16]>();
        let (_, _, id, size) = self.model.reports();
        let mut report = [0; 27];
        report[0] = id;
        report[1..7].copy_from_slice(&host.address.hci_bytes());
        report[7..23].copy_from_slice(&key);
        self.device.set_feature(&report[..size])?;
        let after = self.read_pairing()?;
        if after.controller_address != before.controller_address
            || after.host_address != Some(host.address)
        {
            return Err(Error::InvalidPacket(
                "Sony cable pairing readback did not match; retry cable pairing",
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
