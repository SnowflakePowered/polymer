use super::plugin::{PackageError, PluginPackage, confined_path};
use crate::{EmulatorPluginInstance, EmulatorPort, PluginInstance, PluginModuleLoader};
use deno_core::ModuleSpecifier;
use serde::Deserialize;
use snowglass_config::{
    ConfigurationToml,
    ast::{ConfigurationDefinition, ConfigurationInstance, PortAssignments},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use toml_spanner::Arena;

/// Keys owned directly by the `[emulator]` block.
#[derive(Debug, Deserialize)]
struct EmulatorBlock {
    platforms: Vec<String>,
    main: PathBuf,
    configuration: ConfigurationBlock,
}

/// Keys owned by the `[emulator.configuration]` block.
#[derive(Debug, Deserialize)]
struct ConfigurationBlock {
    definitions: Vec<PathBuf>,
}

/// A loaded emulator plugin. It owns the generic plugin package and its
/// emulator-specific configuration definition, but no JavaScript runtime or
/// viewer launch policy.
pub struct EmulatorPlugin {
    package: PluginPackage<EmulatorBlock>,
    module_loader: PluginModuleLoader,
    entrypoint: ModuleSpecifier,
    definition: Arc<ConfigurationDefinition>,
}

impl EmulatorPlugin {
    pub fn load(root: impl AsRef<Path>) -> Result<Self, PackageError> {
        let package = PluginPackage::<EmulatorBlock>::load(root)?;
        if package.class() != "emulator" {
            return Err(PackageError::UnexpectedClass {
                expected: "emulator",
                actual: package.class().to_owned(),
            });
        }
        let emulator = package.block();
        let entrypoint_path = confined_path(package.root(), &emulator.main)?;
        let module_loader = package.module_loader();
        let entrypoint = module_loader
            .specifier(&entrypoint_path)
            .map_err(|error| PackageError::ModuleSpecifier(error.to_string()))?;

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

        Ok(Self {
            package,
            module_loader,
            entrypoint,
            definition,
        })
    }

    pub fn root(&self) -> &Path {
        self.package.root()
    }

    pub fn id(&self) -> &str {
        self.package.id()
    }

    pub fn version(&self) -> &str {
        self.package.version()
    }

    pub fn class(&self) -> &str {
        self.package.class()
    }

    pub fn config(&self) -> &toml::Table {
        self.package.config()
    }

    pub fn module_loader(&self) -> &PluginModuleLoader {
        &self.module_loader
    }

    pub fn entrypoint(&self) -> &ModuleSpecifier {
        &self.entrypoint
    }

    pub fn platforms(&self) -> &[String] {
        &self.package.block().platforms
    }

    pub fn ports(&self, console: &str) -> Result<Vec<EmulatorPort>, PackageError> {
        let input = self
            .definition
            .input
            .as_ref()
            .ok_or(PackageError::NoInputDefinition)?;
        let platform = input
            .platforms
            .get(console)
            .ok_or_else(|| PackageError::UnknownPlatform(console.to_owned()))?;
        Ok(platform
            .iter()
            .flat_map(|(group, ports)| {
                ports.iter().map(|port| EmulatorPort {
                    group: group.to_string(),
                    index: port.index,
                    layouts: port.layouts.iter().map(ToString::to_string).collect(),
                    pointer: snowglass_input_layout::topology::PointerMode::Relative,
                })
            })
            .collect())
    }

    /// Maps viewer pad layouts onto the unique compatible port group in order.
    pub fn port_assignments(
        &self,
        console: &str,
        layouts: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> Result<PortAssignments, PackageError> {
        let input = self
            .definition
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
            values.push(layout.into());
            assignments.insert(group.clone(), values.into_boxed_slice());
        }
        Ok(assignments)
    }

    pub fn materialize(
        &self,
        console: &str,
        assignments: PortAssignments,
    ) -> Result<ConfigurationInstance, PackageError> {
        Ok(self.definition.instantiate(console, assignments)?)
    }
}

impl PluginInstance for EmulatorPlugin {
    fn id(&self) -> &str {
        self.id()
    }

    fn version(&self) -> &str {
        self.version()
    }

    fn class(&self) -> &str {
        self.class()
    }

    fn module_loader(&self) -> &PluginModuleLoader {
        self.module_loader()
    }

    fn resources(&self) -> Result<Vec<crate::PluginResource>, PackageError> {
        self.package.resources()
    }

    fn caches(&self) -> &[crate::PluginCache] {
        self.package.caches()
    }
}

impl EmulatorPluginInstance for EmulatorPlugin {
    fn entrypoint(&self) -> &ModuleSpecifier {
        self.entrypoint()
    }

    fn configuration_definition(&self) -> &ConfigurationDefinition {
        &self.definition
    }

    fn platforms(&self) -> &[String] {
        self.platforms()
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
