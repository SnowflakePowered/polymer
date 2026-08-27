//! Nintendo Switch 2 BLE protocol layered over the generic GATT transport.

use std::time::Duration;

use aes::{
    Aes128,
    cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray},
};
use async_trait::async_trait;
use btleplug::api::WriteType;
use bytes::Bytes;
use serde_json::Value;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    DeviceAddr, Error, Result, Transport,
    devices::{ConnectOptions, ControllerDevice, DeviceIdentity, binary_player_leds},
    transport::ble::{self, BleDevice, GattProfile},
};

pub const SERVICE_UUID: Uuid = Uuid::from_u128(0xab7de9be_89fe_49ad_828f_118f09df7fd0);
const INPUT_UUID: Uuid = Uuid::from_u128(0xab7de9be_89fe_49ad_828f_118f09df7fd2);
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Switch2Kind {
    JoyConLeft,
    JoyConRight,
    ProController,
    NsoGameCube,
    #[default]
    Unknown,
}

impl Switch2Kind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::JoyConLeft => "Joy-Con 2 (L)",
            Self::JoyConRight => "Joy-Con 2 (R)",
            Self::ProController => "Nintendo Switch 2 Pro Controller",
            Self::NsoGameCube => "Nintendo GameCube Controller",
            Self::Unknown => "Nintendo Switch 2 controller",
        }
    }

    const fn input_uuid(self) -> Uuid {
        match self {
            Self::JoyConLeft => JOYCON_LEFT_INPUT_UUID,
            Self::JoyConRight => JOYCON_RIGHT_INPUT_UUID,
            Self::ProController => PRO_INPUT_UUID,
            Self::NsoGameCube => GAMECUBE_INPUT_UUID,
            Self::Unknown => INPUT_UUID,
        }
    }

    const fn feature_mask(self) -> u8 {
        match self {
            Self::JoyConLeft | Self::JoyConRight => 0x37,
            Self::ProController => 0x2f,
            Self::NsoGameCube | Self::Unknown => 0x27,
        }
    }

    pub const fn code(self) -> u8 {
        match self {
            Self::JoyConLeft => 1,
            Self::JoyConRight => 2,
            Self::ProController => 3,
            Self::NsoGameCube => 4,
            Self::Unknown => 0,
        }
    }

    const fn from_code(code: u8) -> Self {
        match code {
            1 => Self::JoyConLeft,
            2 => Self::JoyConRight,
            3 => Self::ProController,
            4 => Self::NsoGameCube,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Switch2Device {
    kind: Switch2Kind,
}

impl Switch2Device {
    pub const fn new(kind: Switch2Kind) -> Self {
        Self { kind }
    }

    pub fn gatt_profile_for(kind: Switch2Kind) -> GattProfile {
        GattProfile {
            service: SERVICE_UUID,
            input: kind.input_uuid(),
            input_fallbacks: vec![INPUT_UUID],
            output: COMMAND_UUID,
            response: Some(RESPONSE_UUID),
            output_write_type: WriteType::WithoutResponse,
            idle_disconnect_timeout: Some(Duration::from_secs(3)),
        }
    }

    pub fn kind_for(device: &BleDevice) -> Switch2Kind {
        Self::manufacturer_data(device)
            .and_then(Self::kind_from_manufacturer_data)
            .or_else(|| {
                let name = device.name.as_deref()?;
                [
                    Switch2Kind::JoyConLeft,
                    Switch2Kind::JoyConRight,
                    Switch2Kind::ProController,
                    Switch2Kind::NsoGameCube,
                ]
                .into_iter()
                .find(|kind| name.eq_ignore_ascii_case(kind.label()))
            })
            .unwrap_or_default()
    }

    pub fn is_pairing_mode(device: &BleDevice) -> bool {
        Self::manufacturer_data(device).is_some_and(Self::is_pairing_data)
    }

    pub fn bonded_host(device: &BleDevice) -> Option<DeviceAddr> {
        Self::bonded_host_from_data(Self::manufacturer_data(device)?)
    }

    pub async fn scan(adapter_index: usize, duration: Duration) -> Result<Vec<BleDevice>> {
        let devices = ble::scan(adapter_index, duration).await?;
        Ok(devices
            .into_iter()
            .filter(Self::matches_advertisement)
            .collect())
    }

    fn effective_kind(self, state: &Value) -> Switch2Kind {
        if self.kind == Switch2Kind::Unknown {
            state
                .get("switch2_kind")
                .and_then(Value::as_u64)
                .map_or(self.kind, |code| Switch2Kind::from_code(code as u8))
        } else {
            self.kind
        }
    }

    async fn command(
        &self,
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

    pub async fn bond(
        &self,
        transport: &mut dyn Transport,
        host_address: DeviceAddr,
    ) -> Result<()> {
        let mut reversed_host_address = host_address.bytes();
        reversed_host_address.reverse();
        let mut address_payload = [0; 14];
        address_payload[1] = 2;
        address_payload[2..8].copy_from_slice(&reversed_host_address);
        address_payload[8..14].copy_from_slice(&reversed_host_address);
        let address_response = self
            .command(transport, 0x15, 0x01, &address_payload)
            .await?;
        if address_response.len() < 9 || address_response[0] != 1 || address_response[2] != 1 {
            return Err(Error::InvalidPacket(
                "Switch 2 pairing address response is malformed",
            ));
        }
        let host_key_wire = rand::random::<[u8; 16]>();
        let mut key_request = [0; 17];
        key_request[1..].copy_from_slice(&host_key_wire);
        let key_response = self.command(transport, 0x15, 0x04, &key_request).await?;
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
        let challenge_response = self
            .command(transport, 0x15, 0x02, &challenge_request)
            .await?;
        if challenge_response.len() < 17
            || challenge_response[0] != 1
            || challenge_response[1..17] != Self::pairing_confirmation(ltk_wire, challenge_wire)
        {
            return Err(Error::InvalidPacket(
                "Switch 2 pairing AES confirmation failed",
            ));
        }
        if self.command(transport, 0x15, 0x03, &[0]).await?.first() != Some(&1) {
            return Err(Error::InvalidPacket(
                "Switch 2 pairing finalization was rejected",
            ));
        }
        Ok(())
    }

    pub async fn set_led_flashing(
        &self,
        transport: &mut dyn Transport,
        enabled: bool,
    ) -> Result<()> {
        self.command(transport, 0x09, 0x08, &[u8::from(enabled), 0, 0, 0])
            .await?;
        Ok(())
    }

    fn manufacturer_data(device: &BleDevice) -> Option<&[u8]> {
        device
            .manufacturer_data
            .get(&NINTENDO_MANUFACTURER_ID)
            .map(Vec::as_slice)
    }

    fn identity_manufacturer_data(identity: &DeviceIdentity) -> Option<&[u8]> {
        identity
            .manufacturer_data
            .iter()
            .find_map(|(id, data)| (*id == NINTENDO_MANUFACTURER_ID).then_some(data.as_slice()))
    }

    fn is_pairing_data(data: &[u8]) -> bool {
        data.len() >= 16 && data[10..16].iter().all(|byte| *byte == 0)
    }

    fn bonded_host_from_data(data: &[u8]) -> Option<DeviceAddr> {
        if data.len() < 16 || Self::is_pairing_data(data) {
            return None;
        }
        let mut bytes: [u8; 6] = data[10..16].try_into().ok()?;
        bytes.reverse();
        Some(DeviceAddr::public(bytes))
    }

    fn kind_from_manufacturer_data(data: &[u8]) -> Option<Switch2Kind> {
        if data.len() < 5 || data[..4] != ADVERTISEMENT_PREFIX {
            return None;
        }
        for pair in data[4..].windows(2) {
            match u16::from_le_bytes([pair[0], pair[1]]) {
                0x2067 => return Some(Switch2Kind::JoyConLeft),
                0x2066 | 0x2068 => return Some(Switch2Kind::JoyConRight),
                0x2069 => return Some(Switch2Kind::ProController),
                0x2073 => return Some(Switch2Kind::NsoGameCube),
                _ => {}
            }
        }
        Some(Switch2Kind::Unknown)
    }

    fn matches_advertisement(device: &BleDevice) -> bool {
        let kind = device
            .manufacturer_data
            .get(&NINTENDO_MANUFACTURER_ID)
            .and_then(|data| Self::kind_from_manufacturer_data(data));
        let advertised_service = device.services.contains(&SERVICE_UUID);
        let nintendo_name = device.name.as_ref().is_some_and(|name| {
            let name = name.to_ascii_lowercase();
            name.contains("joy-con") || name.contains("pro controller") || name.contains("gamecube")
        });
        kind.is_some() || advertised_service || nintendo_name
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

#[async_trait]
impl ControllerDevice for Switch2Device {
    fn gatt_profile(
        &self,
        identity: &DeviceIdentity,
        state: &mut Value,
    ) -> Result<Option<GattProfile>> {
        let kind = Self::identity_manufacturer_data(identity)
            .and_then(Self::kind_from_manufacturer_data)
            .unwrap_or_default();
        state["switch2_kind"] = Value::from(kind.code());
        Ok(Some(Self::gatt_profile_for(kind)))
    }

    fn accepts_reconnect_advertisement(
        &self,
        identity: &DeviceIdentity,
        local_address: Option<DeviceAddr>,
    ) -> bool {
        let Some(data) = Self::identity_manufacturer_data(identity) else {
            return false;
        };
        local_address.map_or_else(
            || Self::bonded_host_from_data(data).is_some(),
            |address| Self::bonded_host_from_data(data) == Some(address),
        )
    }

    async fn bond(
        &self,
        transport: &mut dyn Transport,
        identity: &DeviceIdentity,
        local_address: DeviceAddr,
        state: &mut Value,
    ) -> Result<()> {
        let data = Self::identity_manufacturer_data(identity).ok_or(Error::InvalidPacket(
            "Switch 2 advertisement lacks Nintendo manufacturer data",
        ))?;
        if !Self::is_pairing_data(data) {
            return Ok(());
        }
        Switch2Device::new(self.effective_kind(state))
            .bond(transport, local_address)
            .await?;
        Ok(())
    }

    async fn sync(
        &self,
        transport: &mut dyn Transport,
        options: &ConnectOptions,
        state: &mut Value,
    ) -> Result<()> {
        let kind = self.effective_kind(state);
        transport.send(&SPI_INIT).await?;
        self.command(transport, 0x0c, 0x02, &[kind.feature_mask(), 0, 0, 0])
            .await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        self.command(transport, 0x0c, 0x04, &[kind.feature_mask(), 0, 0, 0])
            .await?;
        self.set_player_led(transport, options.player_index, &mut Value::Null)
            .await
    }

    async fn set_player_led(
        &self,
        transport: &mut dyn Transport,
        slot: u8,
        _state: &mut Value,
    ) -> Result<()> {
        let mut data = [0; 8];
        data[0] = binary_player_leds(slot);
        self.command(transport, 0x09, 0x07, &data).await?;
        Ok(())
    }

    async fn sleep(&self, transport: &mut dyn Transport, _state: &mut Value) -> Result<()> {
        transport
            .send(&[
                0x06, 0x91, 0x01, 0x02, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ])
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn recognizes_advertisement_product_ids() {
        let mut data = vec![0x01, 0x00, 0x03, 0x7e, 0xaa];
        data.extend_from_slice(&0x2067u16.to_le_bytes());
        assert_eq!(
            Switch2Device::kind_from_manufacturer_data(&data),
            Some(Switch2Kind::JoyConLeft)
        );
        data[5..7].copy_from_slice(&0x2073u16.to_le_bytes());
        assert_eq!(
            Switch2Device::kind_from_manufacturer_data(&data),
            Some(Switch2Kind::NsoGameCube)
        );
    }

    #[test]
    fn distinguishes_pairing_and_reconnect_advertisements() {
        let mut manufacturer_data = HashMap::new();
        manufacturer_data.insert(
            NINTENDO_MANUFACTURER_ID,
            vec![1, 0, 3, 0x7e, 5, 0x66, 0x20, 0, 1, 0, 0, 0, 0, 0, 0, 0],
        );
        let mut device = BleDevice {
            addr: DeviceAddr::public([1, 2, 3, 4, 5, 6]),
            name: None,
            rssi: None,
            services: Vec::new(),
            manufacturer_data,
        };
        assert!(Switch2Device::is_pairing_mode(&device));
        let host = DeviceAddr::public([0x60, 0xff, 0x9e, 0x87, 0xf0, 0x8f]);
        device
            .manufacturer_data
            .get_mut(&NINTENDO_MANUFACTURER_ID)
            .unwrap()[10..16]
            .copy_from_slice(&[0x8f, 0xf0, 0x87, 0x9e, 0xff, 0x60]);
        assert_eq!(Switch2Device::bonded_host(&device), Some(host));

        let identity = DeviceIdentity {
            address: device.addr,
            transport: crate::TransportKind::Ble,
            name: device.name.clone(),
            class_of_device: None,
            service_uuids: device.services.clone(),
            manufacturer_data: device
                .manufacturer_data
                .iter()
                .map(|(id, data)| (*id, data.clone()))
                .collect(),
            pnp: None,
        };
        let facade = Switch2Device::default();
        assert!(facade.accepts_reconnect_advertisement(&identity, Some(host)));
        assert!(!facade.accepts_reconnect_advertisement(
            &identity,
            Some(DeviceAddr::public([1, 1, 1, 1, 1, 1]))
        ));
    }

    #[test]
    fn facade_selects_model_specific_gatt_profile() {
        let mut data = vec![0x01, 0x00, 0x03, 0x7e, 0xaa];
        data.extend_from_slice(&0x2067u16.to_le_bytes());
        let identity = DeviceIdentity {
            address: DeviceAddr::public([1, 2, 3, 4, 5, 6]),
            transport: crate::TransportKind::Ble,
            name: None,
            class_of_device: None,
            service_uuids: vec![SERVICE_UUID],
            manufacturer_data: vec![(NINTENDO_MANUFACTURER_ID, data)],
            pnp: None,
        };
        let mut state = Value::Object(Default::default());
        let profile = Switch2Device::default()
            .gatt_profile(&identity, &mut state)
            .unwrap()
            .unwrap();
        assert_eq!(profile.input, JOYCON_LEFT_INPUT_UUID);
        assert_eq!(state["switch2_kind"], Value::from(1));
    }

    #[test]
    fn verifies_documented_pairing_confirmation_vector() {
        let host_key = [
            0x35, 0x03, 0xe9, 0x29, 0x82, 0x87, 0x71, 0x24, 0xbe, 0xa8, 0x0c, 0x66, 0x46, 0x15,
            0x83, 0x4b,
        ];
        let device_key = [
            0x5c, 0xf6, 0xee, 0x79, 0x2c, 0xdf, 0x05, 0xe1, 0xba, 0x2b, 0x63, 0x25, 0xc4, 0x1a,
            0x5f, 0x10,
        ];
        let challenge = [
            0x6f, 0xc6, 0xdf, 0x8a, 0xd8, 0xfe, 0xdf, 0x15, 0xbb, 0x8c, 0x15, 0xe9, 0x1f, 0x32,
            0x05, 0x44,
        ];
        let mut ltk = [0; 16];
        for index in 0..16 {
            ltk[index] = host_key[index] ^ device_key[index];
        }
        assert_eq!(
            Switch2Device::pairing_confirmation(ltk, challenge),
            [
                0x13, 0x4c, 0x97, 0xf5, 0x11, 0xb9, 0xb6, 0xdd, 0x4d, 0x86, 0xfd, 0x40, 0xf5, 0x36,
                0xe9, 0xed,
            ]
        );
    }
}
