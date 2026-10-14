use serde::Deserialize;

use crate::{PackageError, PluginClass, PluginManifest};

#[derive(Deserialize)]
pub struct PlatformBlock {
    platforms: Vec<String>,
}

impl PluginClass for PlatformBlock {
    const CLASS: &'static str = "platform";

    fn prepare(manifest: &mut PluginManifest<Self>) -> Result<(), PackageError> {
        manifest.source().require_file(manifest.main())?;
        let platforms = &manifest.block().platforms;
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

impl PluginManifest<PlatformBlock> {
    pub fn platforms(&self) -> &[String] {
        &self.block().platforms
    }
}
