use polymer_controllers::{ControllerMapping, ControllerTopology, ControllerTopologyChange};

use polymer_config::input::{InputConfiguration, InputTransaction};

/// A validated configuration change and the emulator topology it produces.
/// The controller manager remains unchanged until this value is committed.
pub struct ControllerTopologyTransaction {
    pub(crate) input: InputTransaction,
    previous_configuration: InputConfiguration,
    candidate_configuration: InputConfiguration,
    previous_topology: ControllerTopology,
    requested_topology: ControllerTopology,
}

impl ControllerTopologyTransaction {
    pub(crate) fn new(
        input: InputTransaction,
        previous_configuration: InputConfiguration,
        candidate_configuration: InputConfiguration,
        previous_topology: ControllerTopology,
        requested_topology: ControllerTopology,
    ) -> Self {
        Self {
            input,
            previous_configuration,
            candidate_configuration,
            previous_topology,
            requested_topology,
        }
    }

    pub fn previous_configuration(&self) -> &InputConfiguration {
        &self.previous_configuration
    }

    pub fn candidate_configuration(&self) -> &InputConfiguration {
        &self.candidate_configuration
    }

    pub fn previous_topology(&self) -> &ControllerTopology {
        &self.previous_topology
    }

    pub fn requested_topology(&self) -> &ControllerTopology {
        &self.requested_topology
    }

    pub fn topology_changed(&self) -> bool {
        self.previous_topology != self.requested_topology
    }

    pub fn adopt_rebinding(&mut self) -> bool {
        let previous = &self.previous_topology.mappings;
        let requested = &mut self.requested_topology.mappings;
        let rebinding = previous.len() == requested.len()
            && previous
                .iter()
                .zip(requested.iter())
                .all(|(previous, requested)| {
                    ControllerMapping {
                        projection_mode: requested.projection_mode,
                        orientation: requested.orientation,
                        mapping: requested.mapping.clone(),
                        pointer: requested.pointer,
                        ..previous.clone()
                    } == *requested
                });
        if rebinding {
            for (requested, previous) in requested.iter_mut().zip(previous) {
                requested.pointer = previous.pointer;
            }
        }
        rebinding
    }

    pub fn change(&self) -> ControllerTopologyChange {
        ControllerTopologyChange {
            previous: self.previous_topology.clone(),
            requested: self.requested_topology.clone(),
        }
    }
}
