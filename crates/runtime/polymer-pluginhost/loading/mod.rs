//! Plugin package discovery and registration.

mod descriptor;
mod module_loader;
pub(crate) mod package;
mod registry;

pub use descriptor::{PluginCacheDescriptor, PluginDescriptor, PluginOptionDescriptor};
pub use module_loader::PluginModuleLoader;
pub use package::{PackageError, PluginCache, PluginConfig, PluginConfigError, PluginOption};
pub use registry::{PluginRegistry, PluginRegistryError};

pub(crate) use package::{EmulatorPlugin, PlatformPlugin, ScraperPlugin, ThemePlugin};
