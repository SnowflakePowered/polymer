//! Polymer IPC transports.

pub mod control;
pub mod input;

pub mod shared;

#[cfg(windows)]
pub use control::Win32Resources;
pub use control::{BlobKind, LocalSocket, LocalSocketListener, Message, StreamReady, WireFeedback};

#[cfg(target_os = "macos")]
pub mod xpc;
