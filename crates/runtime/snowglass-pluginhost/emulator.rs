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
    #[serde(default)]
    pub input_hooks: Vec<InputHook>,
    #[serde(default)]
    pub input_providers: Vec<InputProvider>,
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
    pub controller_topology: Option<snowglass_input::topology::ControllerTopology>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsHook {
    Opengl,
    Vulkan,
    D3d11,
    D3d12,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputHook {
    BluetoothApiWin32,
}

/// An emulator-plugin-selected native input path. The host implements the
/// transport but does not infer which devices should use it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum InputProvider {
    BluetoothHidPipe {
        source_id: String,
        port: u8,
        product_id: u16,
    },
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
}

pub type EmulatorPort = snowglass_input::topology::ControllerPort;

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

#[cfg(test)]
mod tests {
    use super::InputProvider;

    #[test]
    fn native_input_provider_uses_the_plugin_camel_case_contract() {
        let provider: InputProvider = serde_json::from_value(serde_json::json!({
            "type": "bluetoothHidPipe",
            "sourceId": "bt://br-edr/00:11:22:33:44:55",
            "port": 2,
            "productId": 0x0330,
        }))
        .unwrap();
        assert!(matches!(
            provider,
            InputProvider::BluetoothHidPipe {
                port: 2,
                product_id: 0x0330,
                ..
            }
        ));
    }
}
