use std::{fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use uuid::Uuid;

use crate::transport::ble::GattProfile;
use crate::{DeviceAddr, Result, Transport, TransportKind};

#[derive(
    Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct ProfileId(String);

impl ProfileId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct PnpIdentity {
    pub vendor_id_source: u16,
    pub vendor_id: u16,
    pub product_id: u16,
    pub version: u16,
}

#[derive(Clone, Debug)]
pub struct DeviceIdentity {
    pub address: DeviceAddr,
    pub transport: TransportKind,
    pub name: Option<String>,
    pub class_of_device: Option<u32>,
    pub service_uuids: Vec<Uuid>,
    pub manufacturer_data: Vec<(u16, Vec<u8>)>,
    pub pnp: Option<PnpIdentity>,
}

impl DeviceIdentity {
    pub fn name_is(&self, expected: &str) -> bool {
        self.name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(expected))
    }

    pub fn name_starts_with(&self, prefix: &str) -> bool {
        self.name
            .as_deref()
            .is_some_and(|name| name.starts_with(prefix))
    }

    pub fn has_service(&self, service: Uuid) -> bool {
        self.service_uuids.contains(&service)
    }

    pub fn manufacturer_data(&self, company_id: u16) -> Option<&[u8]> {
        self.manufacturer_data
            .iter()
            .find_map(|(id, data)| (*id == company_id).then_some(data.as_slice()))
    }

    pub fn has_pnp(&self, vendor_id: u16, product_ids: &[u16]) -> bool {
        self.pnp
            .as_ref()
            .is_some_and(|pnp| pnp.vendor_id == vendor_id && product_ids.contains(&pnp.product_id))
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MatchConfidence {
    Generic = 10,
    Family = 100,
    Model = 200,
    Pnp = 300,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct BondingMode {
    pub id: String,
    pub label: String,
}

impl BondingMode {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PairingPolicy {
    #[default]
    SecureSimplePairing,
    FixedPin(&'static [u8]),
    RemoteAddressPin,
    LocalAddressPin,
    PseudoOob,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconnectPolicy {
    Manual,
    HostInitiated { backoff: Duration },
    ControllerInitiated,
    WakeAdvertisement { backoff: Duration },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConnectionPolicy {
    pub pairing: PairingPolicy,
    pub reconnect: ReconnectPolicy,
    pub controller_opens_hid: bool,
    pub wait_for_encryption_before_inbound_hid: bool,
    pub become_master: bool,
    pub automatic_flush_slots: Option<u16>,
    pub host_identity: HostIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostIdentity {
    pub name: &'static str,
    pub class_of_device: [u8; 3],
}

impl HostIdentity {
    pub const SNOWGLASS: Self = Self {
        name: "Snowglass",
        class_of_device: [0x0c, 0x01, 0x1c],
    };
    pub const NINTENDO: Self = Self {
        name: "Nintendo",
        class_of_device: [0x0c, 0x01, 0x1c],
    };
}

impl Default for ConnectionPolicy {
    fn default() -> Self {
        Self {
            pairing: PairingPolicy::SecureSimplePairing,
            reconnect: ReconnectPolicy::Manual,
            controller_opens_hid: false,
            wait_for_encryption_before_inbound_hid: false,
            become_master: true,
            automatic_flush_slots: None,
            host_identity: HostIdentity::SNOWGLASS,
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct ConnectOptions {
    pub bonding_mode: Option<String>,
    pub player_index: u8,
    pub auto_reconnect: bool,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            bonding_mode: None,
            player_index: 0,
            auto_reconnect: true,
        }
    }
}

#[async_trait]
pub trait ControllerSession: Send {
    async fn synchronize(&mut self, _transport: &mut dyn Transport) -> Result<()> {
        Ok(())
    }

    /// Transfers protocol configuration authority while Snowglass retains the
    /// physical Bluetooth link.
    async fn set_external_ownership(
        &mut self,
        transport: &mut dyn Transport,
        enabled: bool,
    ) -> Result<()> {
        if enabled {
            Ok(())
        } else {
            self.synchronize(transport).await
        }
    }

    async fn set_player_led(&mut self, _transport: &mut dyn Transport, _slot: u8) -> Result<()> {
        Err(crate::Error::Unsupported(
            "controller does not expose player LEDs",
        ))
    }

    async fn set_pairing_indicator(
        &mut self,
        _transport: &mut dyn Transport,
        _enabled: bool,
    ) -> Result<()> {
        Err(crate::Error::Unsupported(
            "controller does not expose a pairing indicator",
        ))
    }

    async fn sleep(&mut self, _transport: &mut dyn Transport) -> Result<()> {
        Err(crate::Error::Unsupported(
            "controller does not expose a sleep command",
        ))
    }

    async fn set_rumble(
        &mut self,
        _transport: &mut dyn Transport,
        _low_frequency: u16,
        _high_frequency: u16,
    ) -> Result<()> {
        Err(crate::Error::Unsupported(
            "controller rumble is not implemented for this profile",
        ))
    }

    async fn set_ir_camera(
        &mut self,
        _transport: &mut dyn Transport,
        _setup: IrCameraSetup,
    ) -> Result<()> {
        Err(crate::Error::Unsupported(
            "controller does not expose an IR camera",
        ))
    }

    async fn send_raw_hid(&mut self, transport: &mut dyn Transport, report: &[u8]) -> Result<()> {
        transport.send(report).await
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IrSensitivity {
    Level1,
    Level2,
    #[default]
    Level3,
    Level4,
    Level5,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IrMode {
    #[default]
    Basic,
    Extended,
    Full,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IrCameraSetup {
    pub sensitivity: IrSensitivity,
    pub mode: IrMode,
}

struct DefaultControllerSession;

#[async_trait]
impl ControllerSession for DefaultControllerSession {}

#[async_trait]
pub trait ControllerDevice: Send + Sync {
    fn id(&self) -> ProfileId;
    fn display_name(&self) -> &'static str;
    fn matches(&self, identity: &DeviceIdentity) -> Option<MatchConfidence>;

    /// Whether this profile can establish its native protocol over a
    /// transport. Classic HID controllers default to BR/EDR; profiles which
    /// implement a BLE GATT data plane opt into BLE explicitly.
    fn supports_transport(&self, transport: TransportKind) -> bool {
        transport == TransportKind::BrEdr
    }

    fn bonding_modes(&self) -> Vec<BondingMode> {
        Vec::new()
    }

    fn connection_policy(
        &self,
        _identity: &DeviceIdentity,
        _options: &ConnectOptions,
    ) -> ConnectionPolicy {
        ConnectionPolicy::default()
    }

    fn gatt_profile(&self, _identity: &DeviceIdentity) -> Result<Option<GattProfile>> {
        Ok(None)
    }

    fn accepts_reconnect_advertisement(
        &self,
        _identity: &DeviceIdentity,
        _local_address: Option<DeviceAddr>,
    ) -> bool {
        true
    }

    fn preferred_input_report_id(&self) -> Option<u8> {
        None
    }

    /// A model-specific BLE input characteristic which identifies this
    /// controller after GATT discovery. Family fallback characteristics may
    /// deliberately identify only the family profile.
    fn input_characteristic_uuid(&self) -> Option<Uuid> {
        None
    }

    fn connect(&self, _options: &ConnectOptions) -> Box<dyn ControllerSession> {
        Box::new(DefaultControllerSession)
    }

    async fn bond(
        &self,
        _transport: &mut dyn Transport,
        _identity: &DeviceIdentity,
        _local_address: DeviceAddr,
    ) -> Result<()> {
        Err(crate::Error::Unsupported(
            "controller does not expose a userspace bonding protocol",
        ))
    }
}

#[derive(Clone, Default)]
pub struct ControllerRegistry {
    controllers: Vec<Arc<dyn ControllerDevice>>,
}

impl ControllerRegistry {
    pub fn standard() -> Self {
        Self {
            controllers: super::catalog::controllers(),
        }
    }

    pub fn register(&mut self, controller: Arc<dyn ControllerDevice>) {
        self.controllers
            .retain(|registered| registered.id() != controller.id());
        self.controllers.push(controller);
    }

    pub fn get(&self, id: &ProfileId) -> Option<Arc<dyn ControllerDevice>> {
        self.controllers
            .iter()
            .find(|controller| controller.id() == *id)
            .cloned()
    }

    pub fn identify(&self, identity: &DeviceIdentity) -> Result<Option<Arc<dyn ControllerDevice>>> {
        let mut matches = self
            .controllers
            .iter()
            .filter(|controller| controller.supports_transport(identity.transport))
            .filter_map(|controller| {
                controller
                    .matches(identity)
                    .map(|confidence| (confidence, controller.clone()))
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|(confidence, _)| *confidence);
        let Some((best, controller)) = matches.pop() else {
            return Ok(None);
        };
        if matches
            .last()
            .is_some_and(|(confidence, _)| confidence == &best)
        {
            return Err(crate::Error::AmbiguousProfile(identity.address));
        }
        Ok(Some(controller))
    }

    pub fn identify_input_characteristic(
        &self,
        uuid: Uuid,
    ) -> Result<Option<Arc<dyn ControllerDevice>>> {
        let mut matches = self
            .controllers
            .iter()
            .filter(|controller| controller.input_characteristic_uuid() == Some(uuid));
        let controller = matches.next().cloned();
        if matches.next().is_some() {
            return Err(crate::Error::Unsupported(
                "BLE input characteristic matches multiple controller profiles",
            ));
        }
        Ok(controller)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wiimote_identity(transport: TransportKind) -> DeviceIdentity {
        DeviceIdentity {
            address: DeviceAddr::public([1, 2, 3, 4, 5, 6]),
            transport,
            name: Some("Nintendo RVL-CNT-01".to_owned()),
            class_of_device: None,
            service_uuids: Vec::new(),
            manufacturer_data: Vec::new(),
            pnp: None,
        }
    }

    #[test]
    fn classic_only_profile_is_not_selected_from_ble_discovery() {
        let registry = ControllerRegistry::standard();
        assert!(
            registry
                .identify(&wiimote_identity(TransportKind::Ble))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            registry
                .identify(&wiimote_identity(TransportKind::BrEdr))
                .unwrap()
                .unwrap()
                .id()
                .as_str(),
            "nintendo.wiimote.original"
        );
    }

    #[test]
    fn fresh_wiimote_pairing_uses_host_initiated_hid_channels() {
        let identity = wiimote_identity(TransportKind::BrEdr);
        let controller = ControllerRegistry::standard()
            .identify(&identity)
            .unwrap()
            .unwrap();
        assert!(
            !controller
                .connection_policy(&identity, &ConnectOptions::default())
                .controller_opens_hid
        );
    }
}
