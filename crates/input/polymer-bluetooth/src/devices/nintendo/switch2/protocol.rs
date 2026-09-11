//! Common Switch 2 pseudo-OOB pairing, used over BLE and USB.
//! Protocol: https://github.com/ndeadly/switch2_controller_research/blob/master/commands.md
use super::packet::CommandPacket;
use crate::{DeviceAddr, Error, Result, Transport};
use aes::{
    Aes128,
    cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray},
};
use async_trait::async_trait;
use bytes::Bytes;
use std::time::Duration;
use tokio::sync::broadcast;

#[async_trait]
pub(crate) trait Switch2Commands: Send {
    async fn command(&mut self, command: u8, subcommand: u8, data: &[u8]) -> Result<Bytes>;

    fn controller_address(response: &[u8]) -> Result<DeviceAddr> {
        if response.len() < 9 || response[0] != 1 || response[2] != 1 {
            return Err(Error::InvalidPacket(
                "Switch 2 pairing address response is malformed",
            ));
        }
        let mut bytes: [u8; 6] = response[3..9].try_into().unwrap();
        bytes.reverse();
        if bytes == [0; 6] || bytes == [255; 6] {
            return Err(Error::InvalidPacket(
                "Switch 2 controller address is invalid",
            ));
        }
        Ok(DeviceAddr::public(bytes))
    }
    async fn pair(&mut self, host_address: DeviceAddr) -> Result<DeviceAddr> {
        let mut reversed_host_address = host_address.bytes();
        reversed_host_address.reverse();
        let mut address_payload = [0; 14];
        address_payload[1] = 2;
        address_payload[2..8].copy_from_slice(&reversed_host_address);
        address_payload[8..14].copy_from_slice(&reversed_host_address);
        let address_response = self.command(0x15, 0x01, &address_payload).await?;
        let controller_address = Self::controller_address(&address_response)?;

        let host_key_wire = rand::random::<[u8; 16]>();
        let mut key_request = [0; 17];
        key_request[1..].copy_from_slice(&host_key_wire);
        let key_response = self.command(0x15, 0x04, &key_request).await?;
        if key_response.len() < 17 || key_response[0] != 1 {
            return Err(Error::InvalidPacket(
                "Switch 2 pairing key response is malformed",
            ));
        }
        let mut ltk_wire = [0; 16];
        for (index, slot) in ltk_wire.iter_mut().enumerate() {
            *slot = host_key_wire[index] ^ key_response[index + 1];
        }

        let challenge_wire = rand::random::<[u8; 16]>();
        let mut challenge_request = [0; 17];
        challenge_request[1..].copy_from_slice(&challenge_wire);
        let challenge_response = self.command(0x15, 0x02, &challenge_request).await?;
        if challenge_response.len() < 17
            || challenge_response[0] != 1
            || challenge_response[1..17] != Self::pairing_confirmation(ltk_wire, challenge_wire)
        {
            return Err(Error::InvalidPacket(
                "Switch 2 pairing AES confirmation failed",
            ));
        }
        if self.command(0x15, 0x03, &[0]).await?.first() != Some(&1) {
            return Err(Error::InvalidPacket(
                "Switch 2 pairing finalization was rejected",
            ));
        }
        Ok(controller_address)
    }

    fn pairing_confirmation(ltk_wire: [u8; 16], challenge_wire: [u8; 16]) -> [u8; 16] {
        let mut key = ltk_wire;
        key.reverse();
        let mut challenge = challenge_wire;
        challenge.reverse();
        let cipher = Aes128::new(GenericArray::from_slice(&key));
        let mut block = GenericArray::clone_from_slice(&challenge);
        cipher.encrypt_block(&mut block);
        let mut response = [0; 16];
        response.copy_from_slice(&block);
        response
    }
}

pub(crate) struct BleCommands<'a>(pub &'a mut dyn Transport);

#[async_trait]
impl Switch2Commands for BleCommands<'_> {
    async fn command(&mut self, command: u8, subcommand: u8, data: &[u8]) -> Result<Bytes> {
        let mut responses = self.0.responses().ok_or(Error::Unsupported(
            "Switch 2 command response notifications are unavailable",
        ))?;
        self.0
            .send(&CommandPacket::new(command, subcommand, 1).encode(data)?)
            .await?;
        let request = CommandPacket::new(command, subcommand, 1);
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match responses.recv().await {
                    Ok(response) => {
                        if let Some(data) = request.response(&response)? {
                            return Ok(data);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return Err(Error::ChannelClosed),
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout("Switch 2 command response"))?
    }
}
