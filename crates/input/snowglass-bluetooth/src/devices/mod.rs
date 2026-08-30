//! Data-driven controller matching, policy, and connection-time behavior.

mod catalog;
mod generic;
pub mod nintendo;
mod profile;
pub mod sony;

pub use profile::{
    BondingMode, ConnectOptions, ConnectionPolicy, ControllerDevice, ControllerRegistry,
    ControllerSession, DeviceIdentity, HostIdentity, IrCameraSetup, IrMode, IrSensitivity,
    MatchConfidence, PairingPolicy, PnpIdentity, ProfileId, ReconnectPolicy,
};
