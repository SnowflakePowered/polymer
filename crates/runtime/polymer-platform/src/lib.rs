//! Shared contracts for a native guest session. Native OS objects stay in backends.

#![forbid(unsafe_code)]

mod audio;
mod types;

pub use audio::{AudioState, NormalizedVolume};
pub use polymer_protocol::{GraphicsHook, Hooks, InputHook};
use std::{ffi::OsString, io, path::PathBuf};
pub use types::{
    GuestInput, GuestWindowId, HotkeyChord, ProcessArchitecture, ProcessExit, ProcessExitReceiver,
    ProcessId,
};

/// Platform launch mechanics consume a command inside the mandatory VFS mount.
pub struct GuestLaunchRequest {
    pub session: String,
    pub executable: polymer_vfs::VfsPathBuf,
    pub arguments: Vec<String>,
    pub environment: Vec<(OsString, OsString)>,
    pub working_directory: Option<PathBuf>,
    pub artifact_root: PathBuf,
    pub hooks: Hooks,
    /// Desktop isolation only; filesystem virtualization is always required.
    pub isolate: bool,
    pub render_configuration: polymer_protocol::RenderConfiguration,
}

/// Owns process supervision, desktop isolation, window input, and process audio.
/// Thread-affine resources are dispatched to backend-owned threads internally.
pub trait Guest: Send {
    fn id(&self) -> ProcessId;
    fn architecture(&self) -> ProcessArchitecture;
    fn subscribe_exit(&self) -> io::Result<ProcessExitReceiver>;
    fn send_input(&self, input: GuestInput) -> io::Result<()>;
    fn audio_state(&self) -> io::Result<AudioState>;
    fn set_volume(&self, volume: NormalizedVolume) -> io::Result<()>;
    fn set_muted(&self, muted: bool) -> io::Result<()>;
    /// Stops the guest tree before releasing its desktop and audio resources.
    /// Repeated calls are harmless; Drop must also attempt cleanup.
    fn terminate_tree_and_wait(&mut self) -> io::Result<()>;
}
