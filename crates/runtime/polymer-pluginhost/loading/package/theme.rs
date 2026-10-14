use super::plugin::PluginPackage;
use crate::theme::ThemePluginInstance;
use polymer_pluginmanifest::{PackageSource, PluginManifest, ThemeBlock, ThemeDebug};
use std::path::Path;

pub type ThemePlugin = PluginPackage<ThemeBlock>;

impl ThemePluginInstance for ThemePlugin {
    fn source(&self) -> &PackageSource {
        PluginManifest::source(self)
    }

    fn main(&self) -> &str {
        PluginManifest::main(self)
    }

    fn debug(&self) -> Option<&ThemeDebug> {
        PluginManifest::debug(self)
    }

    fn theme_root(&self) -> Option<&Path> {
        PluginManifest::theme_root(self)
    }

    fn assets(&self) -> Option<&Path> {
        PluginManifest::assets(self)
    }
}
