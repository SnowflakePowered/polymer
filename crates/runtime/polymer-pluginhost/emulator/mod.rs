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
    /// Presentation mode. D3D11, D3D12, and Vulkan require native swapchains.
    #[serde(default)]
    pub render_mode: polymer_presentation_config::RenderMode,
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
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub quick_resume_screenshot: Option<Vec<u8>>,
}

impl EmulatorLaunchArguments {
    pub fn validate_render_configuration(&self) -> Result<(), String> {
        use polymer_presentation_config::{RenderConfiguration, SurfaceRequest};
        for hook in &self.hooks.graphics {
            hook.validate_render_mode(self.render_mode)?;
        }
        RenderConfiguration {
            render_mode: self.render_mode,
            surfaces: self
                .surfaces
                .iter()
                .map(|surface| SurfaceRequest {
                    index: surface.index,
                    pattern: surface.pattern.clone(),
                    resolution: surface.resolution,
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
    Gdi,
}

impl GraphicsHook {
    pub fn validate_render_mode(
        self,
        mode: polymer_presentation_config::RenderMode,
    ) -> Result<(), String> {
        use polymer_presentation_config::RenderMode;
        let supported = match self {
            Self::D3d11 | Self::D3d12 | Self::Vulkan => mode == RenderMode::Swapchain,
            Self::OpenGl => matches!(mode, RenderMode::Intercept | RenderMode::GraphTracking),
            Self::Gdi => mode == RenderMode::Intercept,
        };
        if supported {
            Ok(())
        } else {
            Err(format!(
                "{} does not support renderMode '{}'; D3D11, D3D12, and Vulkan require swapchain mode with fixed surface resolutions",
                self.label(),
                mode.as_str()
            ))
        }
    }

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
    #[serde(default)]
    pub resolution: Option<[u32; 2]>,
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
