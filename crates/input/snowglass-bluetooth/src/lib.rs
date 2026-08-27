//! Bluetooth controller transports for Snowglass.

#![forbid(unsafe_code)]

pub mod ble;
pub mod bredr;
mod error;
mod transport;
mod types;

pub use error::{Error, Result};
pub use transport::{ConnectionState, ReportReceiver, Transport};
pub use types::{AddressKind, DeviceAddr};
