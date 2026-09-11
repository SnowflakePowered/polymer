//! Bluetooth Classic (BR/EDR) host stack.

mod control;
pub mod hci;
mod hid;
pub mod l2cap;
mod pairing;
pub mod sdp;

pub use hid::BrEdrHid;
pub use pairing::{FileLinkKeyStore, LinkKey, LinkKeyStore, PairingAgent, PinCode};
