//! Switch Pro USB pairing via subcommand 0x01, stages 1/2/3.
//! Reference: dekuNukem/Nintendo_Switch_Reverse_Engineering (USB and SPI notes).
use super::{CablePairingHost, CablePairingState, VerifiedCablePairing, feature::FeaturePairing};
use crate::{DeviceAddr, Error, Result, transport::bredr::LinkKey};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

pub(super) mod usb;
use usb::{Commands, InterruptReports};

pub(super) struct Pairing<D: InterruptReports> {
    commands: Commands<D>,
}

#[derive(FromBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
struct Bond {
    magic: u8,
    length: u8,
    checksum: [u8; 2],
    host: [u8; 6],
    key: [u8; 16],
}
const _: () = assert!(std::mem::size_of::<Bond>() == 26);

impl<D: InterruptReports> Pairing<D> {
    pub fn new(device: D) -> Self {
        Self {
            commands: Commands::new(device),
        }
    }

    fn controller_address(&mut self) -> Result<DeviceAddr> {
        let response = self.commands.subcommand(2, &[])?;
        if response.len() < 12 || response[2] != 3 {
            return Err(Error::InvalidPacket(
                "USB device is not a Switch Pro controller",
            ));
        }
        let address: [u8; 6] = response[4..10].try_into().unwrap();
        if address == [0; 6] || address == [255; 6] {
            return Err(Error::InvalidPacket("invalid Switch Pro address"));
        }
        Ok(DeviceAddr::public(address))
    }

    fn saved_bond(&mut self) -> Result<Option<(DeviceAddr, [u8; 16])>> {
        // Entries have a four-byte header followed by 0x22 bytes of payload.
        for address in (0x2000..=0x3000 - 0x26).step_by(0x26) {
            let bytes = self.commands.read_spi(address, 26)?;
            let bond = Bond::ref_from_bytes(&bytes)
                .map_err(|_| Error::InvalidPacket("invalid Switch bond record"))?;
            if bond.magic == 0xff {
                return Ok(None);
            }
            if !matches!(bond.magic, 0 | 0x95) || bond.length != 0x22 {
                return Err(Error::InvalidPacket("malformed Switch pairing storage"));
            }
            if bond.magic == 0x95 {
                if bond.host == [0; 6] || bond.host == [255; 6] {
                    return Err(Error::InvalidPacket("invalid saved Switch host"));
                }
                return Ok(Some((DeviceAddr::public(bond.host), bond.key)));
            }
        }
        Ok(None)
    }
}

impl<D: InterruptReports> FeaturePairing for Pairing<D> {
    fn read_pairing(&mut self) -> Result<CablePairingState> {
        Ok(CablePairingState {
            controller_address: self.controller_address()?,
            host_address: self.saved_bond()?.map(|bond| bond.0),
        })
    }

    fn pair(&mut self, host: &CablePairingHost) -> Result<VerifiedCablePairing> {
        host.validate()?;
        if host.adapter_index.is_some() {
            return Err(Error::Unsupported(
                "Switch Pro requires the dedicated BR/EDR adapter",
            ));
        }
        let controller_address = self.controller_address()?;
        let mut request = [1; 7];
        request[1..].copy_from_slice(&host.address.hci_bytes());
        let response = self.commands.subcommand(1, &request)?;
        if response.get(1..7) != Some(controller_address.hci_bytes().as_slice()) {
            return Err(Error::InvalidPacket(
                "Switch pairing controller address mismatch",
            ));
        }
        let response = self.commands.subcommand(1, &[2])?;
        let masked: [u8; 16] = response
            .get(1..17)
            .ok_or(Error::InvalidPacket("short Switch pairing key reply"))?
            .try_into()
            .unwrap();
        let mut key = masked.map(|byte| byte ^ 0xaa);
        self.commands.subcommand(1, &[3])?;
        if self.controller_address()? != controller_address
            || self.saved_bond()? != Some((host.address, key))
        {
            return Err(Error::InvalidPacket(
                "Switch USB pairing readback did not match; retry cable pairing",
            ));
        }
        // USB replies and SPI store the key in little-endian order; BR/EDR
        // Link Key Request Reply requires the reversed byte sequence.
        // Keep the SPI comparison above in its original wire order.
        key.reverse();
        Ok(VerifiedCablePairing {
            state: CablePairingState {
                controller_address,
                host_address: Some(host.address),
            },
            link_key: Some(LinkKey {
                value: key,
                key_type: 0,
            }),
        })
    }
}

#[cfg(test)]
mod tests;
