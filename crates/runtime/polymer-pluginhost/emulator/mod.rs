pub(crate) mod adapter;
pub(crate) mod js;

use crate::{loading::PackageError, plugin::PluginInstance};
use polymer_config::ast::{ConfigurationDefinition, ConfigurationInstance, PortAssignments};
use serde_json::Value;
use std::sync::Arc;
use thiserror::Error;

/// Fully realized arguments returned by an emulator plugin. Pluginhost owns
/// this contract, while the viewer decides how to install and launch it.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmulatorLaunchArguments {
    pub command: String,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub isolate: bool,
    pub hooks: Hooks,
    #[serde(default)]
    pub surfaces: Vec<TrackedSurface>,
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub prepared_filesystem: Option<String>,
    #[serde(skip)]
    pub filesystem: Option<polymer_vfs::PreparedFilesystemLaunch>,
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub controller_session: Option<String>,
    #[serde(skip)]
    pub session: Option<crate::controller::ControllerSession>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_topology: Option<polymer_controllers::ControllerTopology>,
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub save_states: Option<SaveStateConfig>,
    #[serde(default, skip_deserializing)]
    pub quick_resume_accepted: bool,
    #[serde(default, skip_deserializing)]
    pub can_change_disc: bool,
    #[serde(default, skip_deserializing)]
    pub initial_disc_index: u32,
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub quick_resume_screenshot: Option<Vec<u8>>,
}

impl EmulatorLaunchArguments {
    pub fn validate_render_configuration(&self) -> Result<(), String> {
        use polymer_protocol::{RenderConfiguration, SurfaceRequest};
        RenderConfiguration {
            surfaces: self
                .surfaces
                .iter()
                .map(|surface| SurfaceRequest {
                    index: surface.index,
                    pattern: surface.pattern.clone(),
                    rotation: surface.rotation,
                })
                .collect(),
        }
        .validate()
    }
}

/// A graphics presentation hook installed into the target process.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsHook {
    OpenGl,
    Vulkan,
    D3d11,
    D3d12,
}

impl GraphicsHook {
    pub const ALL: [Self; 4] = [Self::Vulkan, Self::OpenGl, Self::D3d12, Self::D3d11];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Vulkan => "Vulkan",
            Self::OpenGl => "OpenGL",
            Self::D3d12 => "D3D12",
            Self::D3d11 => "D3D11",
        }
    }

    pub const fn class(self) -> u8 {
        match self {
            Self::D3d11 => 0,
            Self::D3d12 => 1,
            Self::OpenGl => 2,
            Self::Vulkan => 3,
        }
    }
}

/// An input hook installed into the target process.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputHook {
    Sdl3,
    BluetoothApiWin32,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Hooks {
    pub graphics: Vec<GraphicsHook>,
    pub input: Vec<InputHook>,
}

/// The plugin-facing description of an emulator surface. Viewer-only state,
/// such as a Blitz attachment ID or WebRTC publication, is added downstream.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackedSurface {
    pub index: u32,
    #[serde(default)]
    pub rotation: i32,
    #[serde(default)]
    pub pattern: Option<String>,
}

pub struct EmulatorLaunchInvocation {
    pub plugin: Arc<dyn EmulatorPluginInstance>,
    pub arguments: Value,
    pub configuration: ConfigurationInstance,
    pub storage: crate::filesystem::StorageFactories,
    pub quick_resume: Option<QuickResumeData>,
    pub topology: polymer_controllers::ControllerTopology,
    pub explicit_default: bool,
}

#[derive(Clone, Debug)]
pub struct QuickResumeData {
    pub state: Vec<u8>,
    pub screenshot: Option<Vec<u8>>,
}

pub type EmulatorPort = polymer_controllers::ControllerPort;

/// Emulator capability projected from a registered [`PluginInstance`].
pub trait EmulatorPluginInstance: PluginInstance {
    fn configuration_definition(&self) -> &ConfigurationDefinition;
    fn platforms(&self) -> &[String];
    fn save_states(&self) -> Option<&SaveStateConfig>;
    fn ports(&self, console: &str) -> Result<Vec<EmulatorPort>, PackageError>;
    fn port_assignments(
        &self,
        console: &str,
        layouts: &[String],
    ) -> Result<PortAssignments, PackageError>;
    fn materialize(
        &self,
        console: &str,
        assignments: PortAssignments,
    ) -> Result<ConfigurationInstance, PackageError>;
}

#[derive(Debug, Error)]
pub enum EmulatorHostError {
    #[error("plugin runtime failed: {0}")]
    Runtime(String),
    #[error(transparent)]
    Configuration(#[from] crate::configuration::ConfigurationAccessError),
}

pub use crate::loading::package::SaveStateConfig;
