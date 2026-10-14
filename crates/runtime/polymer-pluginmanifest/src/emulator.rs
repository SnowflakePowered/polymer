use polymer_config::{ConfigurationToml, ast::ConfigurationDefinition};
use serde::Deserialize;
use std::{path::PathBuf, sync::Arc};
use toml_spanner::Arena;

use crate::{PackageError, PackageSource, PluginClass, PluginManifest};

/// Keys owned directly by the `[emulator]` block.
#[derive(Deserialize)]
pub struct EmulatorBlock {
    platforms: Vec<String>,
    configuration: ConfigurationBlock,
    #[serde(default)]
    savestates: Option<SaveStateConfig>,
    #[serde(skip)]
    definition: Option<Arc<ConfigurationDefinition>>,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SaveStateConfig {
    pub max: std::num::NonZeroU32,
    #[serde(default)]
    pub revision: Option<String>,
}

/// Keys owned by the `[emulator.configuration]` block.
#[derive(Debug, Deserialize)]
struct ConfigurationBlock {
    definitions: Vec<PathBuf>,
}

impl PluginClass for EmulatorBlock {
    const CLASS: &'static str = "emulator";

    fn prepare(manifest: &mut PluginManifest<Self>) -> Result<(), PackageError> {
        manifest.source().require_file(manifest.main())?;
        let emulator = manifest.block();
        if emulator
            .savestates
            .as_ref()
            .and_then(|states| states.revision.as_deref())
            .is_some_and(|revision| revision.trim().is_empty())
        {
            return Err(PackageError::InvalidSaveStateRevision);
        }
        let mut sources = Vec::new();
        for pattern in &emulator.configuration.definitions {
            let pattern = PackageSource::relative(pattern)?;
            let matches = manifest.source().matching(&pattern)?;
            if matches.is_empty() {
                return Err(PackageError::EmptyDefinitionPattern(pattern));
            }
            for path in matches {
                sources.push(manifest.source().read_to_string(&path)?);
            }
        }
        let arena = Arena::new();
        let parsed = ConfigurationToml::try_from_tomls(&arena, sources.iter().map(String::as_str))?;
        manifest.block_mut().definition = Some(Arc::new(ConfigurationDefinition::from(parsed)));
        Ok(())
    }
}

impl PluginManifest<EmulatorBlock> {
    /// The emulator's configuration definition, parsed from its definition files.
    pub fn definition(&self) -> &Arc<ConfigurationDefinition> {
        self.block()
            .definition
            .as_ref()
            .expect("emulator manifest was prepared during loading")
    }

    pub fn platforms(&self) -> &[String] {
        &self.block().platforms
    }

    pub fn save_states(&self) -> Option<&SaveStateConfig> {
        self.block().savestates.as_ref()
    }
}
