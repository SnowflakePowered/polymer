//! Host-side lifecycle and IPC ownership for an injected emulator process.

#![forbid(unsafe_code)]

mod capture;
mod configuration;
mod consolestorage;
pub mod controllers;
mod frontend;
mod instance;
mod launch;
mod launchable_configuration;
mod library;
mod savestates;
mod session;
mod storage;
mod ui;

pub use capture::{BufferReleaseSender, ConsumerPreference};
pub use configuration::{
    ConfigurationDefaults, ConfigurationRequest, DefaultGamepad, DetectedGame, EmulatorDescriptor,
    GameDescriptor, LaunchGroupRequest, LaunchPortRequest, PortDescriptor, PortGroupDescriptor,
    PreparedGame, describe_api as describe_configuration, prepare_api as prepare_configuration,
};
pub use consolestorage::{manager as console_storage_manager, serve as serve_console_storage};
pub use controllers::{
    ControllerAssignmentSnapshot, ControllerCommandOutcome, ControllerManager,
    ControllerManagerEffect, ControllerProfiles, ControllerSessionCommit,
    ControllerTopologyTransaction, DeviceMapping,
};
pub use frontend::{
    ControllerApiState, Emulator, EmulatorCommand, EmulatorCommandHandler, EmulatorCommandSink,
    FrontendServices, Input, InputCommand, InputCommandHandler, InputCommandSink, WebRtc,
    WebRtcCommand, WebRtcCommandHandler, WebRtcCommandSink,
};
pub use instance::{
    AudioState, EmulatorActionEndpoint, EmulatorInstance, EmulatorLaunchConfig, HotkeyChord,
    HotkeyKey, InstanceExit, InstanceState, LaunchError, NormalizedVolume, PresentedFrameToken,
    ResizeDimensions, SurfaceError, SurfaceEvent, SurfaceEventReceiver, SurfaceFrameReceiver,
    SurfaceFrameUpdate, SurfaceOffer, SurfacePointerUpdate, TrackedSurfaceConfig,
    TrackedSurfaceHandle, TrackedSurfaceId,
};
pub use library::LibraryService;
pub use polymer_command::CommandSink;
pub use polymer_pluginhost::emulator::{GraphicsHook, Hooks, InputHook};
pub use savestates::{RgbaScreenshot, SaveStateManager, SaveStateMetadata};
pub use session::{SessionController, mount_prepared_filesystem};
pub use storage::launch_storage;
pub use ui::{UiRequest, UiRequestReceiver};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeState {
    Uninitialized,
    Connecting,
    Active,
    ShuttingDown,
}

pub use polymer_presentation_config::{RenderConfiguration, RenderMode, SurfaceRequest};
