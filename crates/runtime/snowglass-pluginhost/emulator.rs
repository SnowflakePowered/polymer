use crate::{PackageError, PluginInstance};
use deno_core::ModuleSpecifier;
use serde_json::Value;
use snowglass_config::ast::{ConfigurationInstance, PortAssignments};
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
    pub hooks: Vec<GraphicsHook>,
    /// Process-wide presentation dependency-graph traversal requested by the
    /// emulator plugin for its tracked surfaces.
    #[serde(default)]
    pub graph_tracking: bool,
    pub surfaces: Vec<TrackedSurface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_filesystem: Option<String>,
    #[serde(skip)]
    pub filesystem: Option<snowglass_vfs::PreparedFilesystemLaunch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_session: Option<String>,
    #[serde(skip)]
    pub session: Option<crate::ControllerSession>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamepad_mapping: Option<serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsHook {
    Opengl,
    Vulkan,
    D3d11,
    D3d12,
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

pub struct EmulatorSetupInvocation {
    pub plugin: Arc<dyn EmulatorPluginInstance>,
    pub arguments: Value,
    pub configuration: ConfigurationInstance,
    pub storage: crate::filesystem::StorageFactories,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmulatorPort {
    pub group: String,
    pub index: usize,
    pub layouts: Vec<String>,
}

/// Emulator capability projected from a registered [`PluginInstance`].
pub trait EmulatorPluginInstance: PluginInstance {
    fn entrypoint(&self) -> &ModuleSpecifier;
    fn platforms(&self) -> &[String];
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
    Configuration(#[from] crate::ConfigurationAccessError),
}
