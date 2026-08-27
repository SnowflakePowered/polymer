//! Bluetooth Classic (BR/EDR) host stack.

pub mod hci;
mod hid;
pub mod l2cap;
mod pairing;
pub mod switch;
pub mod wiimote;

pub use hid::BrEdrHid;
pub use pairing::{FileLinkKeyStore, LinkKey, LinkKeyStore, PairingAgent, PinCode};
