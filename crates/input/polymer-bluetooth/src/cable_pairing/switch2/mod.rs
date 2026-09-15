//! Pair through the vendor command interface; leave the HID interface to SDL.
use super::{
    CableDeviceIdentity, CablePairingHost, CablePairingSession, CablePairingState,
    VerifiedCablePairing,
};
use crate::{DeviceAddr, Error, Result, devices::nintendo::switch2::protocol::Switch2Commands};
use async_trait::async_trait;

mod usb;
pub(super) use usb::UsbCommands;

pub(super) struct Session<C> {
    identity: CableDeviceIdentity,
    commands: C,
}

impl<C: Switch2Commands> Session<C> {
    pub fn new(identity: CableDeviceIdentity, commands: C) -> Self {
        Self { identity, commands }
    }

    async fn saved_hosts(&mut self) -> Result<Vec<DeviceAddr>> {
        // Only need the count and the two addresses, not the stored keys.
        // https://github.com/ndeadly/switch2_controller_research/blob/master/memory_layout.md
        let request = [0x40, 0x7e, 0, 0, 0, 0xa0, 0x1f, 0];
        let response = self.commands.command(0x02, 0x04, &request).await?;
        if response.len() < 72 || response[0] != 0x40 || response[4..8] != request[4..8] {
            return Err(Error::InvalidPacket(
                "Switch 2 pairing memory response is malformed",
            ));
        }
        let data = &response[8..72];
        let count = match data[0] {
            0xff => 0, // Erased factory flash.
            count @ 0..=2 => usize::from(count),
            _ => {
                return Err(Error::InvalidPacket(
                    "Switch 2 pairing entry count is invalid",
                ));
            }
        };
        Ok((0..count)
            .map(|index| {
                let offset = 8 + index * 0x28;
                // Flash addresses are stored in display order, unlike command payloads.
                DeviceAddr::public(data[offset..offset + 6].try_into().unwrap())
            })
            .collect())
    }
}

#[async_trait]
impl<C: Switch2Commands> CablePairingSession for Session<C> {
    fn identity(&self) -> &CableDeviceIdentity {
        &self.identity
    }

    async fn read_pairing(&mut self) -> Result<CablePairingState> {
        let response = self.commands.command(0x15, 0x01, &[]).await?;
        let controller_address = C::controller_address(&response)?;
        Ok(CablePairingState {
            controller_address,
            host_address: self.saved_hosts().await?.first().copied(),
        })
    }

    async fn pair(&mut self, host: &CablePairingHost) -> Result<VerifiedCablePairing> {
        host.validate()?;
        let before = self.read_pairing().await?;
        let address = self.commands.pair(host.address).await?;
        let after = self.read_pairing().await?;
        if address != before.controller_address
            || after.controller_address != address
            || !self.saved_hosts().await?.contains(&host.address)
        {
            return Err(Error::InvalidPacket(
                "Switch 2 USB pairing readback did not match",
            ));
        }
        Ok(VerifiedCablePairing {
            state: CablePairingState {
                controller_address: address,
                host_address: Some(host.address),
            },
            // Nintendo's pseudo-OOB key is not a BR/EDR link key. The existing
            // OS GATT reconnect path does not use SMP encryption or import it.
            link_key: None,
        })
    }
}
