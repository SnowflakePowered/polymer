//! Windows DsHidMini sideband pairing, independent of the emulated HID mode.
//! Native handles and shared memory stay in this leaf crate.
#![cfg(windows)]

mod device;
mod ipc;
mod protocol;

pub use device::{Device, PairingState};
