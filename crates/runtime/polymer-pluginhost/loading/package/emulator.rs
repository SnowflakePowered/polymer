use super::plugin::{PackageError, PluginClass, PluginPackage, confined_path};
use crate::emulator::{EmulatorPluginInstance, EmulatorPort};
use polymer_config::{
    ConfigurationToml,
    ast::{ConfigurationDefinition, ConfigurationInstance, PortAssignment, PortAssignments},
};
use serde::Deserialize;
use std::{fs, path::PathBuf, sync::Arc};
use toml_spanner::Arena;

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

pub type EmulatorPlugin = PluginPackage<EmulatorBlock>;

impl PluginClass for EmulatorBlock {
    const CLASS: &'static str = "emulator";

    fn prepare(package: &mut PluginPackage<Self>) -> Result<(), PackageError> {
        confined_path(package.root(), package.main())?;
        let emulator = package.block();
        if emulator
            .savestates
            .as_ref()
            .and_then(|states| states.revision.as_deref())
            .is_some_and(|revision| revision.trim().is_empty())
        {
            return Err(PackageError::InvalidSaveStateRevision);
        }
        let mut definition_paths = Vec::new();
        for pattern in &emulator.configuration.definitions {
            let absolute = package.root().join(pattern);
            let pattern = absolute
                .to_str()
                .ok_or_else(|| PackageError::NonUtf8Path(absolute.clone()))?;
            let mut matches = glob::glob(pattern)?.collect::<Result<Vec<_>, _>>()?;
            matches.sort();
            if matches.is_empty() {
                return Err(PackageError::EmptyDefinitionPattern(pattern.to_owned()));
            }
            for path in matches {
                definition_paths.push(confined_path(package.root(), path)?);
            }
        }
        let sources = definition_paths
            .iter()
            .map(fs::read_to_string)
            .collect::<Result<Vec<_>, _>>()?;
        let arena = Arena::new();
        let parsed = ConfigurationToml::try_from_tomls(&arena, sources.iter().map(String::as_str))?;
        let definition = Arc::new(ConfigurationDefinition::from(parsed));
        package.block_mut().definition = Some(definition);
        Ok(())
    }
}

impl PluginPackage<EmulatorBlock> {
    fn definition(&self) -> &Arc<ConfigurationDefinition> {
        self.block()
            .definition
            .as_ref()
            .expect("emulator package was prepared during loading")
    }

    pub fn platforms(&self) -> &[String] {
        &self.block().platforms
    }

    pub fn save_states(&self) -> Option<&SaveStateConfig> {
        self.block().savestates.as_ref()
    }

    pub fn ports(&self, console: &str) -> Result<Vec<EmulatorPort>, PackageError> {
        let input = self
            .definition()
            .input
            .as_ref()
            .ok_or(PackageError::NoInputDefinition)?;
        let platform = input
            .platforms
            .get(console)
            .ok_or_else(|| PackageError::UnknownPlatform(console.to_owned()))?;
        let priority = consolespec::MachineSpec::try_from(console)
            .map(|machine| {
                machine
                    .input_groups()
                    .map(|group| group.name().to_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut ports = platform
            .iter()
            .flat_map(|(group, ports)| {
                ports.iter().map(|port| EmulatorPort {
                    group: group.to_string(),
                    index: port.index,
                    layouts: port.layouts.iter().map(ToString::to_string).collect(),
                    pointer: polymer_controllers::topology::PointerMode::Relative,
                })
            })
            .collect::<Vec<_>>();
        ports.sort_by_key(|port| {
            let rank = priority.iter().position(|group| *group == port.group);
            (rank.unwrap_or(priority.len()), port.index)
        });
        Ok(ports)
    }

    /// Maps viewer pad layouts onto the unique compatible port group in order.
    pub fn port_assignments(
        &self,
        console: &str,
        layouts: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<PortAssignments, PackageError> {
        let input = self
            .definition()
            .input
            .as_ref()
            .ok_or(PackageError::NoInputDefinition)?;
        let platform = input
            .platforms
            .get(console)
            .ok_or_else(|| PackageError::UnknownPlatform(console.to_owned()))?;
        let mut assignments = PortAssignments::new();
        for layout in layouts {
            let layout = layout.as_ref();
            let candidates = platform
                .iter()
                .filter_map(|(group, ports)| {
                    let index = assignments
                        .get(group.as_ref())
                        .map_or(0, |assigned| assigned.len());
                    ports
                        .iter()
                        .find(|port| {
                            port.index == index && port.layouts.iter().any(|v| &**v == layout)
                        })
                        .map(|_| group.clone())
                })
                .collect::<Vec<_>>();
            let [group] = candidates.as_slice() else {
                return Err(PackageError::AmbiguousLayout {
                    console: console.to_owned(),
                    layout: layout.to_owned(),
                    candidates: candidates.iter().map(ToString::to_string).collect(),
                });
            };
            let mut values = assignments
                .remove(group.as_ref())
                .map(Vec::from)
                .unwrap_or_default();
            values.push(PortAssignment {
                layout: layout.into(),
                mouse: false,
            });
            assignments.insert(group.clone(), values.into_boxed_slice());
        }
        Ok(assignments)
    }

    pub fn materialize(
        &self,
        console: &str,
        assignments: PortAssignments,
    ) -> Result<ConfigurationInstance, PackageError> {
        Ok(self.definition().instantiate(console, assignments)?)
    }
}

impl EmulatorPluginInstance for PluginPackage<EmulatorBlock> {
    fn configuration_definition(&self) -> &ConfigurationDefinition {
        self.definition()
    }

    fn platforms(&self) -> &[String] {
        self.platforms()
    }

    fn save_states(&self) -> Option<&SaveStateConfig> {
        self.save_states()
    }

    fn ports(&self, console: &str) -> Result<Vec<EmulatorPort>, PackageError> {
        self.ports(console)
    }

    fn port_assignments(
        &self,
        console: &str,
        layouts: &[String],
    ) -> Result<PortAssignments, PackageError> {
        self.port_assignments(console, layouts)
    }

    fn materialize(
        &self,
        console: &str,
        assignments: PortAssignments,
    ) -> Result<ConfigurationInstance, PackageError> {
        self.materialize(console, assignments)
    }
}
