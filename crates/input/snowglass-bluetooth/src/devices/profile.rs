use std::{collections::HashMap, fmt, sync::Arc, time::Duration};

use async_trait::async_trait;
use serde_json::Value;
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

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MatchConfidence {
    Generic = 10,
    Name = 100,
    Manufacturer = 200,
    Service = 300,
    Pnp = 400,
}

#[derive(Clone, Debug)]
pub enum MatchRule {
    NameEqualsIgnoreCase(String),
    NamePrefix(String),
    Manufacturer(u16),
    Service(Uuid),
    Pnp {
        vendor_id: u16,
        product_ids: Vec<u16>,
    },
    PnpVendor(u16),
    ClassOfDevice {
        mask: u32,
        value: u32,
    },
}

impl MatchRule {
    fn matches(&self, identity: &DeviceIdentity) -> Option<MatchConfidence> {
        match self {
            Self::NameEqualsIgnoreCase(expected) => identity
                .name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(expected))
                .then_some(MatchConfidence::Name),
            Self::NamePrefix(prefix) => identity
                .name
                .as_deref()
                .is_some_and(|name| name.starts_with(prefix))
                .then_some(MatchConfidence::Name),
            Self::Manufacturer(company) => identity
                .manufacturer_data
                .iter()
                .any(|(id, _)| id == company)
                .then_some(MatchConfidence::Manufacturer),
            Self::Service(uuid) => identity
                .service_uuids
                .contains(uuid)
                .then_some(MatchConfidence::Service),
            Self::Pnp {
                vendor_id,
                product_ids,
            } => identity
                .pnp
                .as_ref()
                .is_some_and(|pnp| {
                    pnp.vendor_id == *vendor_id && product_ids.contains(&pnp.product_id)
                })
                .then_some(MatchConfidence::Pnp),
            Self::PnpVendor(vendor_id) => identity
                .pnp
                .as_ref()
                .is_some_and(|pnp| pnp.vendor_id == *vendor_id)
                .then_some(MatchConfidence::Manufacturer),
            Self::ClassOfDevice { mask, value } => identity
                .class_of_device
                .is_some_and(|class| class & mask == *value)
                .then_some(MatchConfidence::Generic),
        }
    }
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

pub trait ControllerProfile: Send + Sync {
    fn id(&self) -> ProfileId;
    fn display_name(&self) -> &'static str;
    fn matches(&self, identity: &DeviceIdentity) -> Option<MatchConfidence>;
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
    fn device(&self) -> &dyn ControllerDevice {
        &GENERIC_DEVICE
    }
}

#[async_trait]
pub trait ControllerDevice: Send + Sync {
    fn gatt_profile(
        &self,
        _identity: &DeviceIdentity,
        _state: &mut Value,
    ) -> Result<Option<GattProfile>> {
        Ok(None)
    }
    fn accepts_reconnect_advertisement(
        &self,
        _identity: &DeviceIdentity,
        _local_address: Option<DeviceAddr>,
    ) -> bool {
        true
    }
    async fn bond(
        &self,
        _transport: &mut dyn Transport,
        _identity: &DeviceIdentity,
        _local_address: DeviceAddr,
        _state: &mut Value,
    ) -> Result<()> {
        Err(crate::Error::Unsupported(
            "controller does not expose a userspace bonding protocol",
        ))
    }
    async fn sync(
        &self,
        _transport: &mut dyn Transport,
        _options: &ConnectOptions,
        _state: &mut Value,
    ) -> Result<()> {
        Ok(())
    }
    async fn reconnect(
        &self,
        transport: &mut dyn Transport,
        address: DeviceAddr,
        options: &ConnectOptions,
        state: &mut Value,
    ) -> Result<()> {
        transport.connect(address).await?;
        self.sync(transport, options, state).await
    }
    async fn set_player_led(
        &self,
        _transport: &mut dyn Transport,
        _slot: u8,
        _state: &mut Value,
    ) -> Result<()> {
        Err(crate::Error::Unsupported(
            "controller does not expose player LEDs",
        ))
    }
    async fn sleep(&self, _transport: &mut dyn Transport, _state: &mut Value) -> Result<()> {
        Err(crate::Error::Unsupported(
            "controller does not expose a sleep command",
        ))
    }
    async fn send_raw_hid(&self, transport: &mut dyn Transport, report: &[u8]) -> Result<()> {
        transport.send(report).await
    }
}

#[derive(Default)]
struct GenericDevice;

static GENERIC_DEVICE: GenericDevice = GenericDevice;

#[async_trait]
impl ControllerDevice for GenericDevice {}

pub struct DeviceProfile {
    id: ProfileId,
    display_name: &'static str,
    rules: Vec<MatchRule>,
    bonding_modes: Vec<BondingMode>,
    policy: ConnectionPolicy,
    bonding_policies: HashMap<String, ConnectionPolicy>,
    device: Arc<dyn ControllerDevice>,
}

impl DeviceProfile {
    pub fn new(id: impl Into<String>, display_name: &'static str) -> Self {
        Self {
            id: ProfileId::new(id),
            display_name,
            rules: Vec::new(),
            bonding_modes: Vec::new(),
            policy: ConnectionPolicy::default(),
            bonding_policies: HashMap::new(),
            device: Arc::new(GenericDevice),
        }
    }
    pub fn with_rules(mut self, rules: impl IntoIterator<Item = MatchRule>) -> Self {
        self.rules.extend(rules);
        self
    }
    pub fn with_bonding_modes(mut self, modes: impl IntoIterator<Item = BondingMode>) -> Self {
        self.bonding_modes.extend(modes);
        self
    }
    pub const fn with_policy(mut self, policy: ConnectionPolicy) -> Self {
        self.policy = policy;
        self
    }
    pub fn with_bonding_policy(
        mut self,
        mode: impl Into<String>,
        policy: ConnectionPolicy,
    ) -> Self {
        self.bonding_policies.insert(mode.into(), policy);
        self
    }
    pub fn with_device(mut self, device: Arc<dyn ControllerDevice>) -> Self {
        self.device = device;
        self
    }
}

#[async_trait]
impl ControllerProfile for DeviceProfile {
    fn id(&self) -> ProfileId {
        self.id.clone()
    }
    fn display_name(&self) -> &'static str {
        self.display_name
    }
    fn matches(&self, identity: &DeviceIdentity) -> Option<MatchConfidence> {
        self.rules
            .iter()
            .filter_map(|rule| rule.matches(identity))
            .max()
    }
    fn bonding_modes(&self) -> Vec<BondingMode> {
        self.bonding_modes.clone()
    }
    fn connection_policy(
        &self,
        _identity: &DeviceIdentity,
        options: &ConnectOptions,
    ) -> ConnectionPolicy {
        options
            .bonding_mode
            .as_ref()
            .and_then(|mode| self.bonding_policies.get(mode))
            .copied()
            .unwrap_or(self.policy)
    }
    fn device(&self) -> &dyn ControllerDevice {
        self.device.as_ref()
    }
}

#[derive(Clone, Default)]
pub struct ProfileRegistry {
    profiles: Vec<Arc<dyn ControllerProfile>>,
}

impl ProfileRegistry {
    pub fn standard() -> Self {
        Self {
            profiles: super::catalog::profiles(),
        }
    }

    pub fn register(&mut self, profile: Arc<dyn ControllerProfile>) {
        self.profiles
            .retain(|registered| registered.id() != profile.id());
        self.profiles.push(profile);
    }

    pub fn get(&self, id: &ProfileId) -> Option<Arc<dyn ControllerProfile>> {
        self.profiles
            .iter()
            .find(|profile| profile.id() == *id)
            .cloned()
    }

    pub fn identify(
        &self,
        identity: &DeviceIdentity,
    ) -> Result<Option<Arc<dyn ControllerProfile>>> {
        let mut matches = self
            .profiles
            .iter()
            .filter_map(|profile| {
                profile
                    .matches(identity)
                    .map(|confidence| (confidence, profile.clone()))
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|(confidence, _)| *confidence);
        let Some((best, profile)) = matches.pop() else {
            return Ok(None);
        };
        if matches
            .last()
            .is_some_and(|(confidence, _)| confidence == &best)
        {
            return Err(crate::Error::AmbiguousProfile(identity.address));
        }
        Ok(Some(profile))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_registry_identifies_wii_by_name() {
        let identity = DeviceIdentity {
            address: "00:11:22:33:44:55".parse().unwrap(),
            transport: TransportKind::BrEdr,
            name: Some("Nintendo RVL-CNT-01-TR".into()),
            class_of_device: None,
            service_uuids: Vec::new(),
            manufacturer_data: Vec::new(),
            pnp: None,
        };
        assert_eq!(
            ProfileRegistry::standard()
                .identify(&identity)
                .unwrap()
                .unwrap()
                .id()
                .as_str(),
            "nintendo.wiimote"
        );
    }

    #[test]
    fn generic_descriptors_select_bonding_policy_without_family_types() {
        let identity = DeviceIdentity {
            address: "00:11:22:33:44:55".parse().unwrap(),
            transport: TransportKind::BrEdr,
            name: Some("Nintendo RVL-CNT-01".into()),
            class_of_device: None,
            service_uuids: Vec::new(),
            manufacturer_data: Vec::new(),
            pnp: None,
        };
        let profile = ProfileRegistry::standard()
            .identify(&identity)
            .unwrap()
            .unwrap();
        let policy = profile.connection_policy(
            &identity,
            &ConnectOptions {
                bonding_mode: Some("one-two".into()),
                ..ConnectOptions::default()
            },
        );
        assert_eq!(policy.pairing, PairingPolicy::RemoteAddressPin);
    }

    #[test]
    fn pnp_identity_refines_sony_and_similar_nintendo_names_are_unambiguous() {
        let registry = ProfileRegistry::standard();
        let sony = DeviceIdentity {
            address: "00:11:22:33:44:55".parse().unwrap(),
            transport: TransportKind::BrEdr,
            name: Some("Wireless Controller".into()),
            class_of_device: Some(0x002508),
            service_uuids: Vec::new(),
            manufacturer_data: Vec::new(),
            pnp: Some(PnpIdentity {
                vendor_id_source: 2,
                vendor_id: 0x054c,
                product_id: 0x09cc,
                version: 0,
            }),
        };
        assert_eq!(
            registry.identify(&sony).unwrap().unwrap().id().as_str(),
            "sony.dualshock4"
        );

        let snes = DeviceIdentity {
            name: Some("SNES Controller".into()),
            pnp: None,
            ..sony
        };
        assert_eq!(
            registry.identify(&snes).unwrap().unwrap().id().as_str(),
            "nintendo.switch.nso-snes"
        );
    }
}
