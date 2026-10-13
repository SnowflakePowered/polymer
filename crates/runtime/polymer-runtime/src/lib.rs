//! Host-side lifecycle and IPC ownership for an injected emulator process.

#![forbid(unsafe_code)]

mod capture;
mod configuration;
mod consolestorage;
pub mod controllers;
pub mod frontend;
mod instance;
mod jobs;
mod launch;
mod launchable_configuration;
mod library;
mod screenshot;
mod session;
mod sources;
mod storage;

pub use capture::ConsumerPreference;
pub use configuration::{
    ConfigurationDefaults, ConfigurationRequest, DefaultGamepad, DetectedGame, EmulatorDescriptor,
    GameDescriptor, LaunchGroupRequest, LaunchPortRequest, PortDescriptor, PortGroupDescriptor,
    PreparedGame, describe_api as describe_configuration,
};
pub use consolestorage::StorageService;
pub use controllers::{
    ControllerAssignmentSnapshot, ControllerCommandOutcome, ControllerManager,
    ControllerManagerEffect, ControllerProfiles, ControllerSessionCommit,
    ControllerTopologyTransaction, DeviceMapping,
};
pub use frontend::{ControllerApiState, FrontendCommands, FrontendServices};
pub use instance::{
    AudioState, EmulatorActionEndpoint, EmulatorInstance, EmulatorLaunchConfig, HotkeyChord,
    InstanceExit, InstanceState, LaunchError, NormalizedVolume, SurfaceError, SurfaceEvent,
    SurfaceEventReceiver, SurfaceFrameReceiver, SurfaceFrameUpdate, SurfaceOffer,
    SurfacePointerUpdate, TrackedSurfaceConfig, TrackedSurfaceHandle, TrackedSurfaceId,
};
pub use jobs::JobService;
pub use library::LibraryService;
pub use polymer_platform::{GraphicsHook, GuestWindowId, Hooks, InputHook};
pub use screenshot::RgbaScreenshot;
pub use session::{SessionController, filesystem_host, mount_prepared_filesystem};
pub use storage::{CheckpointSession, launch_storage};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeState {
    Uninitialized,
    Connecting,
    Active,
    ShuttingDown,
}

pub use polymer_protocol::{RenderConfiguration, SurfaceRequest};
