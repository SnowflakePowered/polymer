use polymer_controllers::{ControllerTopology, ControllerTopologyChange};

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

    pub fn change(&self) -> ControllerTopologyChange {
        ControllerTopologyChange {
            previous: self.previous_topology.clone(),
            requested: self.requested_topology.clone(),
        }
    }
}
