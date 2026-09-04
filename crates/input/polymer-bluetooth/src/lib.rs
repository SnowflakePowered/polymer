//! Bluetooth controller transports for Polymer.

#![forbid(unsafe_code)]

pub mod devices;
mod error;
pub mod firmware;
pub mod manager;
pub mod store;
pub mod transport;
mod types;

pub use error::{Error, Result};
pub use manager::{
    BluetoothManager, ConnectedController, ControllerId, ManagerConfig, ManagerEvent, ScanRequest,
    SessionId,
};
pub use transport::{ConnectionState, DisconnectReceiver, ReportReceiver, Transport};
pub use types::{AddressKind, DeviceAddr, TransportKind};
