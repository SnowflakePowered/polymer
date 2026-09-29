//! WinFSP adapter for Polymer's platform-neutral virtual filesystem.

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::WinFspFilesystemHost;
