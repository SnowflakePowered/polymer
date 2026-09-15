use super::plugin::{PackageError, PluginClass, PluginPackage, confined_path};
use crate::platform::PlatformPluginInstance;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct PlatformBlock {
    platforms: Vec<String>,
}
pub type PlatformPlugin = PluginPackage<PlatformBlock>;

impl PluginClass for PlatformBlock {
    const CLASS: &'static str = "platform";
    fn prepare(package: &mut PluginPackage<Self>) -> Result<(), PackageError> {
        confined_path(package.root(), package.main())?;
        let platforms = &package.block().platforms;
        if platforms.is_empty() {
            return Err(PackageError::UnknownPlatform(String::new()));
        }
        let mut seen = std::collections::BTreeSet::new();
        for platform in platforms {
            if consolespec::MachineSpec::try_from(platform.as_str()).is_err()
                || !seen.insert(platform)
            {
                return Err(PackageError::UnknownPlatform(platform.clone()));
            }
        }
        Ok(())
    }
}

impl PlatformPluginInstance for PlatformPlugin {
    fn platforms(&self) -> &[String] {
        &self.block().platforms
    }
}
