//! FSKit host for the mandatory guest virtual filesystem.
#![cfg(target_os = "macos")]
mod host;
mod server;
mod wire;
pub use host::FsKitFilesystemHost;
mod extension;
