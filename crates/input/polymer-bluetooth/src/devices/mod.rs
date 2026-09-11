//! Data-driven controller matching, policy, and connection-time behavior.

mod catalog;
mod generic;
mod motion;
pub use motion::SensorCalibration;
pub mod nintendo;
mod profile;
pub mod sony;

pub use profile::{
    ConnectOptions, ConnectionPolicy, ControllerDevice, ControllerRegistry, ControllerSession,
    DeviceIdentity, HostIdentity, IrCameraSetup, IrMode, IrSensitivity, MatchConfidence,
    PairingPolicy, PnpIdentity, ProfileId, ReconnectPolicy, VendorProductId,
};
