use super::plugin::PluginPackage;
use crate::platform::PlatformPluginInstance;
use polymer_pluginmanifest::{PlatformBlock, PluginManifest};

pub type PlatformPlugin = PluginPackage<PlatformBlock>;

impl PlatformPluginInstance for PlatformPlugin {
    fn platforms(&self) -> &[String] {
        PluginManifest::<PlatformBlock>::platforms(self)
    }
}
