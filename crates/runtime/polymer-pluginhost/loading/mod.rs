//! Plugin package discovery and registration.

mod descriptor;
mod module_loader;
pub(crate) mod package;
mod registry;

pub use descriptor::{PluginCacheDescriptor, PluginDescriptor};
pub use module_loader::PluginModuleLoader;
pub use package::{PackageError, PluginCache};
pub use registry::{PluginRegistry, PluginRegistryError};

pub(crate) use package::{EmulatorPlugin, ScraperPlugin, ThemePlugin};
