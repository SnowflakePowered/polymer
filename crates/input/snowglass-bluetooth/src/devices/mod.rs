//! Data-driven controller matching, policy, and connection-time behavior.

mod catalog;
pub mod nintendo;
mod profile;

pub use profile::{
    BondingMode, ConnectOptions, ConnectionPolicy, ControllerDevice, ControllerProfile,
    DeviceIdentity, DeviceProfile, HostIdentity, MatchConfidence, MatchRule, PairingPolicy,
    PnpIdentity, ProfileId, ProfileRegistry, ReconnectPolicy,
};

pub(crate) const fn binary_player_leds(slot: u8) -> u8 {
    if slot >= 0x0e { 0x0f } else { slot + 1 }
}
