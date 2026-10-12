//! Theme plugin API.

use crate::plugin::PluginInstance;
use std::path::Path;

pub use crate::loading::package::ThemeDebug;

/// Theme capability projected from a registered plugin package.
pub trait ThemePluginInstance: PluginInstance {
    fn root(&self) -> &Path;
    fn main(&self) -> &Path;
    fn debug(&self) -> Option<&ThemeDebug>;
    fn theme_root(&self) -> Option<&Path>;
    fn assets(&self) -> Option<&Path>;
}
