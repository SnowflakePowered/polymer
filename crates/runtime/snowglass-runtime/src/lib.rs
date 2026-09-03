//! Host-side lifecycle and IPC ownership for an injected emulator process.

#![forbid(unsafe_code)]

mod capture;
mod configuration;
mod consolestorage;
mod frontend;
mod instance;
mod launch;
mod scraping;
mod session;
mod storage;
mod ui;

pub use capture::ConsumerPreference;
pub use configuration::{
    ConfigurationDefaults, ConfigurationRequest, DefaultGamepad, DetectedGame, EmulatorDescriptor,
    GameDescriptor, LaunchGroupRequest, LaunchPortRequest, PortDescriptor, PortGroupDescriptor,
    PreparedGame, describe_api as describe_configuration, prepare_api as prepare_configuration,
};
pub use consolestorage::{manager as console_storage_manager, serve as serve_console_storage};
pub use frontend::{
    CommandRuntimePlatform, CommandSink, ControllerApiState, EmulatorCommand, EmulatorCommandSink,
    FrontendServices, InputCommand, InputCommandSink, RuntimePlatform, WebRtcCommand,
    WebRtcCommandSink,
};
pub use instance::{
    AudioState, EmulatorActionEndpoint, EmulatorInstance, EmulatorLaunchConfig, HotkeyChord,
    HotkeyKey, InstanceExit, InstanceState, LaunchError, NormalizedVolume, PresentedFrameToken,
    ResizeDimensions, SurfaceError, SurfaceEvent, SurfaceEventReceiver, SurfaceFrameReceiver,
    SurfaceFrameUpdate, SurfaceOffer, SurfacePointerUpdate, TrackedSurfaceConfig,
    TrackedSurfaceHandle, TrackedSurfaceId,
};
pub use scraping::{ScrapedPackage, ScrapingError};
pub use session::{SessionController, mount_prepared_filesystem};
pub use snowglass_pluginhost::{GraphicsHook, Hooks, InputHook};
pub use storage::launch_storage;
pub use ui::{UiRequest, UiRequestReceiver};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeState {
    Uninitialized,
    Connecting,
    Active,
    ShuttingDown,
}
