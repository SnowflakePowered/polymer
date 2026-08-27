//! Bluetooth controller transports for Snowglass.

#![forbid(unsafe_code)]

pub mod devices;
mod error;
pub mod firmware;
pub mod manager;
pub mod store;
pub mod transport;
mod types;

pub use error::{Error, Result};
pub use manager::{BluetoothManager, ManagerConfig, ManagerEvent, ScanRequest};
pub use transport::{ConnectionState, ReportReceiver, Transport};
pub use types::{AddressKind, DeviceAddr, TransportKind};
