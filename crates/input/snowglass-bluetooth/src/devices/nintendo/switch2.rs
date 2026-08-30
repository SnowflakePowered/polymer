//! Nintendo Switch 2 BLE protocols.

use std::{marker::PhantomData, time::Duration};

use aes::{
    Aes128,
    cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray},
};
use async_trait::async_trait;
use btleplug::api::WriteType;
use bytes::Bytes;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::devices::{
    BondingMode, ConnectOptions, ConnectionPolicy, ControllerDevice, ControllerSession,
    DeviceIdentity, HostIdentity, MatchConfidence, PairingPolicy, ProfileId, ReconnectPolicy,
};
use crate::transport::ble::{BleDevice, GattProfile};
use crate::{DeviceAddr, Error, Result, Transport, TransportKind};

pub const SERVICE_UUID: Uuid = Uuid::from_u128(0xab7de9be_89fe_49ad_828f_118f09df7fd0);
pub const COMMON_INPUT_UUID: Uuid = Uuid::from_u128(0xab7de9be_89fe_49ad_828f_118f09df7fd2);
const JOYCON_LEFT_INPUT_UUID: Uuid = Uuid::from_u128(0xcc1bbbb5_7354_4d32_a716_a81cb241a32a);
const JOYCON_RIGHT_INPUT_UUID: Uuid = Uuid::from_u128(0xd5a9e01e_2ffc_4cca_b20c_8b67142bf442);
const PRO_INPUT_UUID: Uuid = Uuid::from_u128(0x7492866c_ec3e_4619_8258_32755ffcc0f8);
const GAMECUBE_INPUT_UUID: Uuid = Uuid::from_u128(0x8261cba1_9435_420c_84d6_f0c75a2c8e4d);
const COMMAND_UUID: Uuid = Uuid::from_u128(0x649d4ac9_8eb7_4e6c_af44_1ea54fe5f005);
const RESPONSE_UUID: Uuid = Uuid::from_u128(0xc765a961_d9d8_4d36_a20a_5315b111836a);
const NINTENDO_MANUFACTURER_ID: u16 = 0x0553;
const ADVERTISEMENT_PREFIX: [u8; 4] = [0x01, 0x00, 0x03, 0x7e];
const SPI_INIT: [u8; 16] = [
    0x21, 0x01, 0x00, 0x10, 0x00, 0x18, 0x00, 0x00, 0xd0, 0x3d, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00,
];

#[async_trait]
trait Switch2Model: Send + Sync + 'static {
    const PROFILE_ID: &'static str;
    const DISPLAY_NAME: &'static str;
    const ADVERTISEMENT_NAMES: &'static [&'static str];
    const PRODUCT_IDS: &'static [u16];
    const INPUT_UUID: Uuid;
    const NATIVE_REPORT_ID: u8;
    const FEATURE_MASK: u8;

    fn input_candidates() -> Vec<Uuid> {
        if Self::INPUT_UUID == COMMON_INPUT_UUID {
            vec![COMMON_INPUT_UUID]
        } else {
            vec![Self::INPUT_UUID, COMMON_INPUT_UUID]
        }
    }

    fn matches_switch2(identity: &DeviceIdentity) -> Option<MatchConfidence> {
        let product_match = identity
            .manufacturer_data(NINTENDO_MANUFACTURER_ID)
            .filter(|data| data.starts_with(&ADVERTISEMENT_PREFIX))
            .is_some_and(|data| {
                data[ADVERTISEMENT_PREFIX.len()..]
                    .windows(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .any(|product| Self::PRODUCT_IDS.contains(&product))
            });
        (product_match
            || Self::ADVERTISEMENT_NAMES
                .iter()
                .any(|name| identity.name_is(name)))
        .then_some(MatchConfidence::Model)
    }

    fn gatt_profile() -> GattProfile {
        let mut inputs = Self::input_candidates().into_iter();
        let input = inputs
            .next()
            .expect("Switch 2 models declare at least one input characteristic");
        GattProfile {
            service: SERVICE_UUID,
            input,
            input_fallbacks: inputs.collect(),
            additional_inputs: (Self::NATIVE_REPORT_ID == 0x0a)
                .then_some(COMMON_INPUT_UUID)
                .into_iter()
                .collect(),
            output: COMMAND_UUID,
            response: Some(RESPONSE_UUID),
            output_write_type: WriteType::WithoutResponse,
            idle_disconnect_timeout: Some(Duration::from_secs(3)),
        }
    }

    fn policy() -> ConnectionPolicy {
        ConnectionPolicy {
            pairing: PairingPolicy::PseudoOob,
            reconnect: ReconnectPolicy::WakeAdvertisement {
                backoff: Duration::from_secs(2),
            },
            controller_opens_hid: false,
            wait_for_encryption_before_inbound_hid: false,
            become_master: true,
            automatic_flush_slots: None,
            host_identity: HostIdentity::SNOWGLASS,
        }
    }

    fn is_pairing_data(data: &[u8]) -> bool {
        data.len() >= 16 && data[10..16].iter().all(|byte| *byte == 0)
    }

    fn bonded_host(data: &[u8]) -> Option<DeviceAddr> {
        if data.len() < 16 || Self::is_pairing_data(data) {
            return None;
        }
        let mut bytes: [u8; 6] = data[10..16].try_into().ok()?;
        bytes.reverse();
        Some(DeviceAddr::public(bytes))
    }

    async fn command(
        transport: &mut dyn Transport,
        command: u8,
        subcommand: u8,
        data: &[u8],
    ) -> Result<Bytes> {
        let data_len = u8::try_from(data.len())
            .map_err(|_| Error::InvalidPacket("Switch 2 command payload is too large"))?;
        let mut responses = transport.responses().ok_or(Error::Unsupported(
            "Switch 2 command response notifications are unavailable",
        ))?;
        let mut packet = Vec::with_capacity(8 + data.len());
        packet.extend_from_slice(&[command, 0x91, 0x01, subcommand, 0x00, data_len, 0x00, 0x00]);
        packet.extend_from_slice(data);
        transport.send(&packet).await?;
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match responses.recv().await {
                    Ok(response)
                        if response.len() >= 8
                            && response[0] == command
                            && response[1] == 0x01
                            && response[2] == 0x01
                            && response[3] == subcommand =>
                    {
                        return Ok(Bytes::copy_from_slice(&response[8..]));
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return Err(Error::ChannelClosed),
                }
            }
        })
        .await
        .map_err(|_| Error::Timeout("Switch 2 command response"))?
    }

    async fn pair(transport: &mut dyn Transport, host_address: DeviceAddr) -> Result<()> {
        let mut reversed_host_address = host_address.bytes();
        reversed_host_address.reverse();
        let mut address_payload = [0; 14];
        address_payload[1] = 2;
        address_payload[2..8].copy_from_slice(&reversed_host_address);
        address_payload[8..14].copy_from_slice(&reversed_host_address);
        let address_response = Self::command(transport, 0x15, 0x01, &address_payload).await?;
        if address_response.len() < 9 || address_response[0] != 1 || address_response[2] != 1 {
            return Err(Error::InvalidPacket(
                "Switch 2 pairing address response is malformed",
            ));
        }

        let host_key_wire = rand::random::<[u8; 16]>();
        let mut key_request = [0; 17];
        key_request[1..].copy_from_slice(&host_key_wire);
        let key_response = Self::command(transport, 0x15, 0x04, &key_request).await?;
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
        let challenge_response = Self::command(transport, 0x15, 0x02, &challenge_request).await?;
        if challenge_response.len() < 17
            || challenge_response[0] != 1
            || challenge_response[1..17] != Self::pairing_confirmation(ltk_wire, challenge_wire)
        {
            return Err(Error::InvalidPacket(
                "Switch 2 pairing AES confirmation failed",
            ));
        }
        if Self::command(transport, 0x15, 0x03, &[0]).await?.first() != Some(&1) {
            return Err(Error::InvalidPacket(
                "Switch 2 pairing finalization was rejected",
            ));
        }
        Ok(())
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

    async fn synchronize(transport: &mut dyn Transport, player_index: u8) -> Result<()> {
        transport.send(&SPI_INIT).await?;
        Self::command(transport, 0x0c, 0x02, &[Self::FEATURE_MASK, 0, 0, 0]).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        Self::command(transport, 0x0c, 0x04, &[Self::FEATURE_MASK, 0, 0, 0]).await?;
        Self::set_led(transport, player_index).await
    }

    async fn set_led(transport: &mut dyn Transport, slot: u8) -> Result<()> {
        let mut data = [0; 8];
        data[0] = slot.saturating_add(1).min(0x0f);
        Self::command(transport, 0x09, 0x07, &data).await?;
        Ok(())
    }
}

struct Switch2Session<M> {
    player_index: u8,
    model: PhantomData<M>,
}

#[async_trait]
impl<M: Switch2Model> ControllerSession for Switch2Session<M> {
    async fn synchronize(&mut self, transport: &mut dyn Transport) -> Result<()> {
        M::synchronize(transport, self.player_index).await
    }

    async fn set_player_led(&mut self, transport: &mut dyn Transport, slot: u8) -> Result<()> {
        M::set_led(transport, slot).await
    }

    async fn set_pairing_indicator(
        &mut self,
        transport: &mut dyn Transport,
        enabled: bool,
    ) -> Result<()> {
        M::command(transport, 0x09, 0x08, &[u8::from(enabled), 0, 0, 0]).await?;
        Ok(())
    }

    async fn sleep(&mut self, transport: &mut dyn Transport) -> Result<()> {
        transport
            .send(&[
                0x06, 0x91, 0x01, 0x02, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ])
            .await
    }
}

#[doc(hidden)]
pub struct Switch2Device<M>(PhantomData<M>);

impl<M> Default for Switch2Device<M> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

#[async_trait]
impl<M> ControllerDevice for Switch2Device<M>
where
    M: Switch2Model,
{
    fn id(&self) -> ProfileId {
        ProfileId::new(M::PROFILE_ID)
    }

    fn display_name(&self) -> &'static str {
        M::DISPLAY_NAME
    }

    fn matches(&self, identity: &DeviceIdentity) -> Option<MatchConfidence> {
        M::matches_switch2(identity)
    }

    fn supports_transport(&self, _transport: TransportKind) -> bool {
        true
    }

    fn bonding_modes(&self) -> Vec<BondingMode> {
        vec![BondingMode::new("pseudo-oob", "Nintendo pseudo-OOB")]
    }

    fn connection_policy(
        &self,
        _identity: &DeviceIdentity,
        _options: &ConnectOptions,
    ) -> ConnectionPolicy {
        M::policy()
    }

    fn gatt_profile(&self, _identity: &DeviceIdentity) -> Result<Option<GattProfile>> {
        Ok(Some(M::gatt_profile()))
    }

    fn accepts_reconnect_advertisement(
        &self,
        identity: &DeviceIdentity,
        local_address: Option<DeviceAddr>,
    ) -> bool {
        let Some(data) = identity.manufacturer_data(NINTENDO_MANUFACTURER_ID) else {
            return false;
        };
        local_address.map_or_else(
            || M::bonded_host(data).is_some(),
            |address| M::bonded_host(data) == Some(address),
        )
    }

    fn preferred_input_report_id(&self) -> Option<u8> {
        Some(M::NATIVE_REPORT_ID)
    }

    fn input_characteristic_uuid(&self) -> Option<Uuid> {
        Some(M::INPUT_UUID)
    }

    fn connect(&self, options: &ConnectOptions) -> Box<dyn ControllerSession> {
        Box::new(Switch2Session::<M> {
            player_index: options.player_index,
            model: PhantomData,
        })
    }

    async fn bond(
        &self,
        transport: &mut dyn Transport,
        identity: &DeviceIdentity,
        local_address: DeviceAddr,
    ) -> Result<()> {
        let data =
            identity
                .manufacturer_data(NINTENDO_MANUFACTURER_ID)
                .ok_or(Error::InvalidPacket(
                    "Switch 2 advertisement lacks Nintendo manufacturer data",
                ))?;
        if M::is_pairing_data(data) {
            M::pair(transport, local_address).await?;
        }
        Ok(())
    }
}

controller_model! {
    JoyCon2Left = Switch2Device<JoyCon2LeftModel> impl Switch2Model {
        const PROFILE_ID: &'static str = "nintendo.switch2.joycon-left";
        const DISPLAY_NAME: &'static str = "Joy-Con 2 (L)";
        const ADVERTISEMENT_NAMES: &'static [&'static str] = &["Joy-Con 2 (L)"];
        const PRODUCT_IDS: &'static [u16] = &[0x2067];
        const INPUT_UUID: Uuid = JOYCON_LEFT_INPUT_UUID;
        const NATIVE_REPORT_ID: u8 = 0x07;
        const FEATURE_MASK: u8 = 0x37;
    }
}

controller_model! {
    JoyCon2Right = Switch2Device<JoyCon2RightModel> impl Switch2Model {
        const PROFILE_ID: &'static str = "nintendo.switch2.joycon-right";
        const DISPLAY_NAME: &'static str = "Joy-Con 2 (R)";
        const ADVERTISEMENT_NAMES: &'static [&'static str] = &["Joy-Con 2 (R)"];
        const PRODUCT_IDS: &'static [u16] = &[0x2066, 0x2068];
        const INPUT_UUID: Uuid = JOYCON_RIGHT_INPUT_UUID;
        const NATIVE_REPORT_ID: u8 = 0x08;
        const FEATURE_MASK: u8 = 0x37;
    }
}

controller_model! {
    Switch2ProController = Switch2Device<Switch2ProModel> impl Switch2Model {
        const PROFILE_ID: &'static str = "nintendo.switch2.pro";
        const DISPLAY_NAME: &'static str = "Nintendo Switch 2 Pro Controller";
        const ADVERTISEMENT_NAMES: &'static [&'static str] = &["Nintendo Switch 2 Pro Controller"];
        const PRODUCT_IDS: &'static [u16] = &[0x2069];
        const INPUT_UUID: Uuid = PRO_INPUT_UUID;
        const NATIVE_REPORT_ID: u8 = 0x09;
        const FEATURE_MASK: u8 = 0x2f;
    }
}

controller_model! {
    NsoGameCubeController = Switch2Device<NsoGameCubeModel> impl Switch2Model {
        const PROFILE_ID: &'static str = "nintendo.switch2.nso-gamecube";
        const DISPLAY_NAME: &'static str = "Nintendo GameCube Controller";
        const ADVERTISEMENT_NAMES: &'static [&'static str] = &["Nintendo GameCube Controller"];
        const PRODUCT_IDS: &'static [u16] = &[0x2073];
        const INPUT_UUID: Uuid = GAMECUBE_INPUT_UUID;
        const NATIVE_REPORT_ID: u8 = 0x0a;
        const FEATURE_MASK: u8 = 0x27;
    }
}

controller_model! {
    UnknownSwitch2Controller = Switch2Device<UnknownSwitch2Model> impl Switch2Model {
        const PROFILE_ID: &'static str = "nintendo.switch2.unknown";
        const DISPLAY_NAME: &'static str = "Unknown Nintendo Switch 2 controller";
        const ADVERTISEMENT_NAMES: &'static [&'static str] = &[];
        const PRODUCT_IDS: &'static [u16] = &[];
        const INPUT_UUID: Uuid = COMMON_INPUT_UUID;
        const NATIVE_REPORT_ID: u8 = 0x05;
        const FEATURE_MASK: u8 = 0x27;

        fn matches_switch2(identity: &DeviceIdentity) -> Option<MatchConfidence> {
            let manufacturer = identity
                .manufacturer_data(NINTENDO_MANUFACTURER_ID)
                .is_some_and(|data| data.starts_with(&ADVERTISEMENT_PREFIX));
            let name = identity.name.as_ref().is_some_and(|name| {
                let name = name.to_ascii_lowercase();
                name.contains("joy-con")
                    || name.contains("pro controller")
                    || name.contains("gamecube")
            });
            (manufacturer || identity.has_service(SERVICE_UUID) || name)
                .then_some(MatchConfidence::Family)
        }

        fn input_candidates() -> Vec<Uuid> {
            vec![
                JOYCON_LEFT_INPUT_UUID,
                JOYCON_RIGHT_INPUT_UUID,
                PRO_INPUT_UUID,
                GAMECUBE_INPUT_UUID,
                COMMON_INPUT_UUID,
            ]
        }
    }
}

impl Switch2Device<UnknownSwitch2Model> {
    pub fn is_pairing_mode(device: &BleDevice) -> bool {
        device
            .manufacturer_data
            .get(&NINTENDO_MANUFACTURER_ID)
            .is_some_and(|data| UnknownSwitch2Model::is_pairing_data(data))
    }

    pub fn bonded_host_for(device: &BleDevice) -> Option<DeviceAddr> {
        UnknownSwitch2Model::bonded_host(device.manufacturer_data.get(&NINTENDO_MANUFACTURER_ID)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::ControllerRegistry;

    #[test]
    fn native_characteristic_refines_a_family_level_match() {
        let profile = UnknownSwitch2Model::gatt_profile();
        assert_eq!(profile.input, JOYCON_LEFT_INPUT_UUID);
        assert!(profile.input_fallbacks.contains(&GAMECUBE_INPUT_UUID));
        assert_eq!(
            ControllerRegistry::standard()
                .identify_input_characteristic(GAMECUBE_INPUT_UUID)
                .unwrap()
                .unwrap()
                .id()
                .as_str(),
            "nintendo.switch2.nso-gamecube"
        );
    }
}
