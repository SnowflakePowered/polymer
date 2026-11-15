//! Common public identity for loaded plugins.

use crate::loading::{PluginCache, PluginConfig, PluginModuleLoader};

pub use crate::host::HostApi;

/// Runtime-neutral identity and package services shared by every plugin class.
pub trait PluginInstance: Send + Sync + 'static {
    fn id(&self) -> &str;
    fn version(&self) -> &str;
    fn class(&self) -> &str;
    fn module_loader(&self) -> &PluginModuleLoader;
    fn entrypoint(&self) -> &deno_core::ModuleSpecifier;

    fn caches(&self) -> &[PluginCache] {
        &[]
    }

    fn config(&self) -> &PluginConfig;

    fn meta(&self) -> &polymer_pluginmanifest::PluginMeta;

    /// The host API versions the plugin supports.
    fn api(&self) -> &semver::VersionReq;

    /// The parsed `plugin.toml`.
    fn manifest(&self) -> &serde_json::Value;
}
