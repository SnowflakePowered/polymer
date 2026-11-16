use super::plugin::PluginPackage;
use crate::emulator::{EmulatorPluginInstance, EmulatorPort};
use polymer_config::ast::{
    ConfigurationDefinition, ConfigurationInstance, PortAssignment, PortAssignments,
};
use polymer_pluginmanifest::{EmulatorBlock, PackageError, PluginManifest, SaveStateConfig};

pub type EmulatorPlugin = PluginPackage<EmulatorBlock>;

impl PluginPackage<EmulatorBlock> {
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
        PluginManifest::definition(self)
    }

    fn platforms(&self) -> &[String] {
        PluginManifest::<EmulatorBlock>::platforms(self)
    }

    fn save_states(&self) -> Option<&SaveStateConfig> {
        PluginManifest::save_states(self)
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
