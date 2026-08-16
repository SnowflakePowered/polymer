use crate::{PackageError, PluginInstance};
use deno_core::ModuleSpecifier;
use serde_json::Value;
use snowglass_config::ast::{ConfigurationInstance, PortAssignments};
use std::{collections::BTreeMap, sync::Arc};
use thiserror::Error;

/// Fully realized arguments returned by an emulator plugin. Pluginhost owns
/// this contract, while the viewer decides how to install and launch it.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmulatorLaunchArguments {
    pub command: String,
    #[serde(default)]
    pub argv: Vec<String>,
    pub config_root: String,
    pub config: BTreeMap<String, serde_bytes::ByteBuf>,
    #[serde(default)]
    pub isolate: bool,
    pub hooks: Vec<GraphicsHook>,
    pub surfaces: Vec<TrackedSurface>,
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
    /// JSON-compatible arguments passed to the plugin's `setupLaunch` method.
    /// The runtime adapter adds the invocation-scoped `configuration` proxy.
    pub arguments: Value,
    pub configuration: ConfigurationInstance,
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
