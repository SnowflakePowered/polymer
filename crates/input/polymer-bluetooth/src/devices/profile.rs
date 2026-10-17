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

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct VendorProductId {
    pub vendor_id: u16,
    pub product_id: u16,
}

impl VendorProductId {
    pub const fn new(vendor_id: u16, product_id: u16) -> Self {
        Self {
            vendor_id,
            product_id,
        }
    }
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
    pub fn vendor_product(&self) -> Option<VendorProductId> {
        self.pnp
            .as_ref()
            .map(|pnp| VendorProductId::new(pnp.vendor_id, pnp.product_id))
    }

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
    /// Authenticate and open HID on a controller-initiated ACL, serving SDP probes.
    pub host_opens_hid_on_reconnect: bool,
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
    pub const POLYMER: Self = Self {
        name: "Polymer",
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
            host_opens_hid_on_reconnect: false,
            become_master: true,
            automatic_flush_slots: None,
            host_identity: HostIdentity::POLYMER,
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct ConnectOptions {
    pub player_index: u8,
    pub auto_reconnect: bool,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            player_index: 0,
            auto_reconnect: true,
        }
    }
}

#[async_trait]
pub trait ControllerSession: Send {
    fn sensor_calibration(&self) -> Option<super::SensorCalibration> {
        None
    }
    fn ds3_motion(&self) -> Option<super::sony::dualshock3::motion::MotionState> {
        None
    }

    async fn synchronize(&mut self, _transport: &mut dyn Transport) -> Result<()> {
        Ok(())
    }

    fn serial(&self) -> Option<Arc<str>> {
        None
    }

    /// Transfers protocol configuration authority while Polymer retains the
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

    /// The VID/PID exposed to consumers of this controller. Profiles may
    /// provide canonical identifiers when the transport did not advertise a
    /// Bluetooth Device ID record.
    fn vendor_product(&self, identity: &DeviceIdentity) -> Option<VendorProductId> {
        identity.vendor_product()
    }

    /// The stable serial exposed to input consumers when it is available from
    /// controller identity rather than the protocol session.
    fn serial(&self, _identity: &DeviceIdentity) -> Option<Arc<str>> {
        None
    }

    /// Whether this profile can establish its native protocol over a
    /// transport. Classic HID controllers default to BR/EDR; profiles which
    /// implement a BLE GATT data plane opt into BLE explicitly.
    fn supports_transport(&self, transport: TransportKind) -> bool {
        transport == TransportKind::BrEdr
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
