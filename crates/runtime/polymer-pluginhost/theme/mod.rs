//! Theme plugin API.

use crate::plugin::PluginInstance;
use std::path::Path;

pub use crate::loading::package::ThemeDebug;

/// Theme capability projected from a registered plugin package.
pub trait ThemePluginInstance: PluginInstance {
    fn source(&self) -> &polymer_pluginmanifest::PackageSource;
    fn main(&self) -> &str;
    fn debug(&self) -> Option<&ThemeDebug>;
    fn theme_root(&self) -> Option<&Path>;
    fn assets(&self) -> Option<&Path>;
    fn titlebar(&self) -> bool;
}
