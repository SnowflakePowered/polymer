//! Snowglass IPC transports.

pub mod control;
pub mod frame;
pub mod input;

mod shared;

pub use control::{LocalSocket, LocalSocketListener, Message, StreamReady};
