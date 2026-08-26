//! Host-side lifecycle and IPC ownership for an injected emulator process.

#![forbid(unsafe_code)]

mod capture;
mod configuration;
mod consolestorage;
mod frontend;
mod input;
mod instance;
mod launch;
mod scraping;
mod session;
mod storage;
mod ui;

pub use capture::ConsumerPreference;
pub use configuration::{
    ChoiceDescriptor, ConfigurationDefaults, ConfigurationRequest, DefaultGamepad, DetectedGame,
    EmulatorDescriptor, GameDescriptor, GamepadMapping, LaunchGroupRequest, LaunchPortRequest,
    OptionDescriptor, PortDescriptor, PortGroupDescriptor, PreparedGame,
    describe_api as describe_configuration, prepare_api as prepare_configuration,
};
pub use consolestorage::{manager as console_storage_manager, serve as serve_console_storage};
pub use frontend::{
    CommandRuntimePlatform, CommandSink, EmulatorCommand, EmulatorCommandSink, FrontendServices,
    InputCommand, InputCommandSink, RuntimePlatform, WebRtcCommand, WebRtcCommandSink,
};
#[doc(hidden)]
pub use input::ProtocolInputSender;
pub use instance::{
    AudioState, EmulatorInstance, EmulatorLaunchConfig, GeneratedConfiguration, HotkeyChord,
    HotkeyKey, InstanceExit, InstanceState, LaunchError, NormalizedVolume, PresentedFrameToken,
    ResizeDimensions, SurfaceError, SurfaceEvent, SurfaceEventReceiver, SurfaceFrameReceiver,
    SurfaceFrameUpdate, SurfaceOffer, SurfacePointerUpdate, TrackedSurfaceConfig,
    TrackedSurfaceHandle, TrackedSurfaceId,
};
pub use scraping::{ScrapedPackage, ScrapingError};
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

/// A graphics capture backend installed into the target process.
///
/// Ordering in [`EmulatorLaunchConfig::hooks`] is also the preference order used
/// when multiple hooks expose the same logical surface.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HookBackend {
    Vulkan,
    OpenGl,
    D3d12,
    D3d11,
    Gdi,
}

impl HookBackend {
    pub const ALL: [Self; 5] = [
        Self::Vulkan,
        Self::OpenGl,
        Self::D3d12,
        Self::D3d11,
        Self::Gdi,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Vulkan => "Vulkan",
            Self::OpenGl => "OpenGL",
            Self::D3d12 => "D3D12",
            Self::D3d11 => "D3D11",
            Self::Gdi => "GDI",
        }
    }

    pub const fn class(self) -> u8 {
        match self {
            Self::Gdi => 0,
            Self::D3d11 => 1,
            Self::D3d12 => 2,
            Self::OpenGl => 3,
            Self::Vulkan => 4,
        }
    }
}
