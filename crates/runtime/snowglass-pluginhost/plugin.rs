//! Common public identity for loaded plugins.

use crate::loading::{PackageError, PluginCache, PluginModuleLoader, PluginResource};

/// Runtime-neutral identity and package services shared by every plugin class.
pub trait PluginInstance: Send + Sync + 'static {
    fn id(&self) -> &str;
    fn version(&self) -> &str;
    fn class(&self) -> &str;
    fn module_loader(&self) -> &PluginModuleLoader;
    fn entrypoint(&self) -> &deno_core::ModuleSpecifier;

    fn resources(&self) -> Result<Vec<PluginResource>, PackageError> {
        Ok(Vec::new())
    }

    fn caches(&self) -> &[PluginCache] {
        &[]
    }
}
