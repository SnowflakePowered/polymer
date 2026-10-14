//! Common public identity for loaded plugins.

use crate::loading::{PluginCache, PluginConfig, PluginModuleLoader};

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

    /// The parsed `plugin.toml`.
    fn manifest(&self) -> &serde_json::Value;
}
