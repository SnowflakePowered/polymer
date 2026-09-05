use crate::layout::{InputBinding, InputLayout};
use consolespec::MachineSpec;
use consolespec::input::BindingKey;
use parking_lot::RwLock;
use polymer_config::input::{
    ControllerBindings, InputConfiguration, InputOperation, InputTransaction, PortConfiguration,
    SourceAssignment, SourceSelector, VirtualPort,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    sync::Arc,
};

/// A serializable view of a consolespec input group, for the frontend.
///
/// [`consolespec::machine::InputGroup`] borrows the compile-time table, which is exactly
/// right in the hub and useless over a JSON boundary; this owns its strings.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputGroup {
    pub name: String,
    pub inputs: Vec<String>,
    pub ports: u8,
    pub accessories: bool,
}

impl From<consolespec::machine::InputGroup> for InputGroup {
    fn from(group: consolespec::machine::InputGroup) -> Self {
        Self {
            name: group.name().to_owned(),
            inputs: group.inputs().map(str::to_owned).collect(),
            ports: group.ports(),
            accessories: group.is_accessory_group(),
        }
    }
}

#[derive(Clone)]
pub struct ControllerConfiguration {
    inner: Arc<RwLock<HubState>>,
}

#[derive(Clone)]
struct HubState {
    revision: u64,
    machine: Option<String>,
    ports: BTreeMap<VirtualPort, PortConfiguration>,
    assignments: BTreeMap<u8, SourceSelector>,
}

impl HubState {
    fn configuration(&self) -> InputConfiguration {
        InputConfiguration {
            revision: self.revision,
            machine: self.machine.clone(),
            ports: self.ports.values().cloned().collect(),
            assignments: self
                .assignments
                .iter()
                .map(|(&index, source)| SourceAssignment {
                    index,
                    source: source.clone(),
                })
                .collect(),
        }
    }

    fn prepare(&self, transaction: &InputTransaction) -> Result<Self, InputConfigurationError> {
        if transaction.expected_revision != self.revision {
            return Err(InputConfigurationError::RevisionConflict {
                expected: transaction.expected_revision,
                actual: self.revision,
            });
        }
        let mut next = self.clone();
        for operation in transaction.operations.iter().cloned() {
            next.apply_operation(operation)?;
        }
        validate_assignments(&next.assignments)?;
        validate_ports(next.machine.as_deref(), &next.ports)?;
        next.revision = next.revision.wrapping_add(1);
        Ok(next)
    }

    fn apply_operation(
        &mut self,
        operation: InputOperation,
    ) -> Result<(), InputConfigurationError> {
        match operation {
            InputOperation::ReplacePorts {
                machine,
                ports: replacement,
            } => {
                self.machine = Some(machine);
                self.ports = collect_ports(replacement)?;
            }
            InputOperation::ConfigurePort { port, layout, slot } => {
                self.ports.insert(
                    port.clone(),
                    PortConfiguration {
                        port,
                        layout,
                        slot,
                        force_mouse: false,
                        overrides: ControllerBindings::default(),
                    },
                );
            }
            InputOperation::SetAssignment { index, source } => {
                if let Some(source) = source {
                    self.assignments.insert(index, source);
                } else {
                    self.assignments.remove(&index);
                }
            }
            InputOperation::ReplaceAssignments { assignments } => {
                let mut replacement = BTreeMap::new();
                for assignment in assignments {
                    if replacement
                        .insert(assignment.index, assignment.source)
                        .is_some()
                    {
                        return Err(InputConfigurationError::InvalidBinding(format!(
                            "duplicate physical source assignment for virtual controller {}",
                            assignment.index
                        )));
                    }
                }
                self.assignments = replacement;
            }
            InputOperation::SetLayout { port, layout } => {
                let port = port_mut(&mut self.ports, port)?;
                port.layout = layout;
                // Overrides name targets in the previous topology. Clearing them is
                // deterministic and avoids accidentally applying a same-named
                // control with different semantics in the replacement layout.
                port.overrides.clear();
            }
            InputOperation::SetOverride {
                port,
                target,
                binding,
            } => {
                port_mut(&mut self.ports, port)?
                    .overrides
                    .insert(target, binding);
            }
            InputOperation::ResetTarget { port, target } => {
                port_mut(&mut self.ports, port)?.overrides.remove(target);
            }
            InputOperation::ResetPortOverrides { port } => {
                port_mut(&mut self.ports, port)?.overrides.clear();
            }
            InputOperation::RemovePort { port } => {
                if self.ports.remove(&port).is_none() {
                    return Err(InputConfigurationError::UnknownPort(port));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputConfigurationError {
    RevisionConflict {
        expected: u64,
        actual: u64,
    },
    DuplicatePort(VirtualPort),
    DuplicateSlot(u8),
    UnknownLayout(String),
    UnknownMachine(String),
    /// Ports were configured before any machine was declared.
    NoMachine,
    UnknownGroup {
        machine: String,
        group: String,
    },
    /// A port names a group that hosts accessories rather than controllers.
    AccessoryGroup {
        machine: String,
        group: String,
    },
    PortOutOfRange {
        port: VirtualPort,
        ports: u8,
    },
    LayoutNotAccepted {
        port: VirtualPort,
        layout: String,
    },
    UnknownPort(VirtualPort),
    UnknownTarget {
        layout: String,
        target: BindingKey,
    },
    InvalidBinding(String),
}

impl fmt::Display for InputConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RevisionConflict { expected, actual } => write!(
                formatter,
                "input configuration revision conflict: expected {expected}, actual {actual}"
            ),
            Self::DuplicatePort(port) => write!(formatter, "duplicate virtual port {port}"),
            Self::DuplicateSlot(slot) => {
                write!(formatter, "two ports drive virtual pad slot {slot}")
            }
            Self::UnknownLayout(layout) => write!(formatter, "unknown input layout {layout}"),
            Self::UnknownMachine(machine) => write!(formatter, "unknown machine {machine}"),
            Self::NoMachine => {
                formatter.write_str("ports were configured before a machine was declared")
            }
            Self::UnknownGroup { machine, group } => {
                write!(formatter, "machine {machine} has no input group {group}")
            }
            Self::AccessoryGroup { machine, group } => write!(
                formatter,
                "input group {group} of {machine} hosts accessories, not controllers"
            ),
            Self::PortOutOfRange { port, ports } => write!(
                formatter,
                "port {port} is outside the {} port(s) its group declares",
                ports
            ),
            Self::LayoutNotAccepted { port, layout } => write!(
                formatter,
                "port {port} does not accept input layout {layout}"
            ),
            Self::UnknownPort(port) => write!(formatter, "unknown virtual port {port}"),
            Self::UnknownTarget { layout, target } => {
                write!(formatter, "input layout {layout} has no element {target}")
            }
            Self::InvalidBinding(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for InputConfigurationError {}

impl ControllerConfiguration {
    pub fn new(
        machine: Option<String>,
        ports: impl IntoIterator<Item = PortConfiguration>,
    ) -> Result<Self, InputConfigurationError> {
        let ports = collect_ports(ports)?;
        validate_ports(machine.as_deref(), &ports)?;
        Ok(Self {
            inner: Arc::new(RwLock::new(HubState {
                revision: 0,
                machine,
                ports,
                assignments: BTreeMap::new(),
            })),
        })
    }

    pub fn snapshot(&self) -> InputConfiguration {
        let state = self.inner.read();
        state.configuration()
    }

    /// The attached machine's consolespec entry, for a caller that needs to ask
    /// it something the hub does not expose — which group accepts a layout, say.
    pub fn machine(&self) -> Option<MachineSpec> {
        machine_spec(self.inner.read().machine.as_deref())
    }

    /// The input groups of the attached machine, in declaration order. Empty
    /// before a machine is declared.
    pub fn groups(&self) -> Vec<InputGroup> {
        let state = self.inner.read();
        state
            .machine
            .as_deref()
            .and_then(|machine| MachineSpec::try_from(machine).ok())
            .map(|machine| machine.input_groups().map(InputGroup::from).collect())
            .unwrap_or_default()
    }

    pub fn layouts(&self) -> &'static [InputLayout] {
        InputLayout::all()
    }

    /// Applies all operations or none of them.
    pub fn preview(
        &self,
        transaction: &InputTransaction,
    ) -> Result<InputConfiguration, InputConfigurationError> {
        let state = self.inner.read();
        state.prepare(transaction).map(|next| next.configuration())
    }

    /// Applies all operations or none of them.
    pub fn apply(
        &self,
        transaction: InputTransaction,
    ) -> Result<InputConfiguration, InputConfigurationError> {
        let mut state = self.inner.write();
        let next = state.prepare(&transaction)?;
        *state = next;
        Ok(state.configuration())
    }
}

fn validate_assignments(
    assignments: &BTreeMap<u8, SourceSelector>,
) -> Result<(), InputConfigurationError> {
    let mut sources = HashSet::new();
    for (index, source) in assignments {
        if !sources.insert(source) {
            return Err(InputConfigurationError::InvalidBinding(format!(
                "physical source {source:?} is assigned to more than one virtual controller (including {index})"
            )));
        }
    }
    Ok(())
}

fn collect_ports(
    ports: impl IntoIterator<Item = PortConfiguration>,
) -> Result<BTreeMap<VirtualPort, PortConfiguration>, InputConfigurationError> {
    let mut collected = BTreeMap::new();
    for port in ports {
        let id = port.port.clone();
        if collected.insert(id.clone(), port).is_some() {
            return Err(InputConfigurationError::DuplicatePort(id));
        }
    }
    Ok(collected)
}

fn port_mut(
    ports: &mut BTreeMap<VirtualPort, PortConfiguration>,
    port: VirtualPort,
) -> Result<&mut PortConfiguration, InputConfigurationError> {
    ports
        .get_mut(&port)
        .ok_or(InputConfigurationError::UnknownPort(port))
}

/// The consolespec entry for an attached machine. Lookup is a binary search
/// over a compile-time table, so there is nothing to own and nothing to pass.
fn machine_spec(machine: Option<&str>) -> Option<MachineSpec> {
    MachineSpec::try_from(machine?).ok()
}

fn validate_ports(
    machine: Option<&str>,
    ports: &BTreeMap<VirtualPort, PortConfiguration>,
) -> Result<(), InputConfigurationError> {
    if ports.is_empty() {
        return Ok(());
    }
    let machine = machine.ok_or(InputConfigurationError::NoMachine)?;
    let spec = machine_spec(Some(machine))
        .ok_or_else(|| InputConfigurationError::UnknownMachine(machine.to_owned()))?;

    let mut slots = HashMap::new();
    for port in ports.values() {
        // The machine decides what a port *is*: whether the group exists, how
        // many it has, and which layouts it accepts. Without this the port is a
        // bare integer and a Wii U GamePad fits in a `controller` slot.
        let group = spec.input_group(&port.port.group).ok_or_else(|| {
            InputConfigurationError::UnknownGroup {
                machine: machine.to_owned(),
                group: port.port.group.clone(),
            }
        })?;
        if group.is_accessory_group() && group.ports() == 0 {
            return Err(InputConfigurationError::AccessoryGroup {
                machine: machine.to_owned(),
                group: group.name().to_owned(),
            });
        }
        if port.port.index >= group.ports() {
            return Err(InputConfigurationError::PortOutOfRange {
                port: port.port.clone(),
                ports: group.ports(),
            });
        }
        if !group.accepts(&port.layout) {
            return Err(InputConfigurationError::LayoutNotAccepted {
                port: port.port.clone(),
                layout: port.layout.clone(),
            });
        }
        // Two ports on one virtual pad would have the emulator read one device
        // for both players, which no caller ever means.
        if let Some(slot) = port.slot
            && slots.insert(slot, ()).is_some()
        {
            return Err(InputConfigurationError::DuplicateSlot(slot));
        }

        let layout = InputLayout::get(&port.layout)
            .ok_or_else(|| InputConfigurationError::UnknownLayout(port.layout.clone()))?;
        for (target, source) in port.overrides.iter() {
            if !layout.has(*target) {
                return Err(InputConfigurationError::UnknownTarget {
                    layout: layout.id.clone(),
                    target: *target,
                });
            }
            if let Some(source) = source {
                let binding = layout.custom_binding(*target, *source).ok_or_else(|| {
                    InputConfigurationError::InvalidBinding(format!(
                        "input source '{source}' is not a physical gamepad expression"
                    ))
                })?;
                if matches!(binding, InputBinding::Axis { .. })
                    && !layout.accepts_axis_binding(*target)
                {
                    return Err(InputConfigurationError::InvalidBinding(format!(
                        "element {target} does not accept an axis binding"
                    )));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use consolespec::input::key;

    /// An Xbox: one `controller` group of four ports, accepting one layout.
    const MACHINE: &str = "MICROSOFT_XBOX";

    fn port(index: u8) -> PortConfiguration {
        PortConfiguration {
            port: VirtualPort::new("controller", index),
            layout: "XBOX_CONTROLLER".to_owned(),
            slot: Some(index),
            force_mouse: false,
            overrides: ControllerBindings::default(),
        }
    }

    fn hub_with(
        ports: impl IntoIterator<Item = PortConfiguration>,
    ) -> Result<ControllerConfiguration, InputConfigurationError> {
        ControllerConfiguration::new(Some(MACHINE.to_owned()), ports)
    }

    fn hub() -> ControllerConfiguration {
        hub_with([port(0)]).unwrap()
    }

    /// `ControllerConfiguration` is not `Debug` — it owns a lock — so the error path takes the
    /// error rather than unwrapping the success.
    fn rejected(
        result: Result<ControllerConfiguration, InputConfigurationError>,
    ) -> InputConfigurationError {
        result
            .err()
            .expect("the topology should have been rejected")
    }

    #[test]
    fn transaction_is_atomic_and_revisioned() {
        let hub = hub();
        let snapshot = hub
            .apply(InputTransaction {
                expected_revision: 0,
                operations: vec![
                    InputOperation::SetOverride {
                        port: VirtualPort::new("controller", 0),
                        target: key!(button.a),
                        binding: Some(key!(button.b)),
                    },
                    InputOperation::SetAssignment {
                        index: 0,
                        source: Some(SourceSelector::KeyboardMouse),
                    },
                ],
            })
            .unwrap();
        assert_eq!(snapshot.revision, 1);
        assert_eq!(
            snapshot.assignments[0].source,
            SourceSelector::KeyboardMouse
        );
        assert!(snapshot.ports[0].overrides.contains(key!(button.a)));
    }

    #[test]
    fn stale_transaction_does_not_modify_configuration() {
        let hub = hub();
        hub.apply(InputTransaction {
            expected_revision: 0,
            operations: vec![InputOperation::ResetPortOverrides {
                port: VirtualPort::new("controller", 0),
            }],
        })
        .unwrap();
        let error = hub
            .apply(InputTransaction {
                expected_revision: 0,
                operations: vec![InputOperation::RemovePort {
                    port: VirtualPort::new("controller", 0),
                }],
            })
            .unwrap_err();
        assert_eq!(
            error,
            InputConfigurationError::RevisionConflict {
                expected: 0,
                actual: 1
            }
        );
        assert_eq!(hub.snapshot().ports.len(), 1);
    }

    #[test]
    fn invalid_target_rolls_back_every_operation() {
        let hub = hub();
        let error = hub
            .apply(InputTransaction {
                expected_revision: 0,
                operations: vec![
                    InputOperation::SetAssignment {
                        index: 0,
                        source: Some(SourceSelector::KeyboardMouse),
                    },
                    InputOperation::SetOverride {
                        port: VirtualPort::new("controller", 0),
                        target: key!(button.z),
                        binding: None,
                    },
                ],
            })
            .unwrap_err();
        assert!(matches!(
            error,
            InputConfigurationError::UnknownTarget { .. }
        ));
        assert!(hub.snapshot().assignments.is_empty());
        assert_eq!(hub.snapshot().revision, 0);
    }

    #[test]
    fn projected_gamecube_shoulders_accept_axis_bindings() {
        let gamecube = PortConfiguration {
            port: VirtualPort::new("gamecube", 0),
            layout: "GAMECUBE_CONTROLLER".to_owned(),
            slot: Some(0),
            force_mouse: false,
            overrides: ControllerBindings::new([
                (key!(button.l), Some(key!(trigger.left))),
                (key!(button.r), Some(key!(trigger.right))),
            ]),
        };

        ControllerConfiguration::new(Some("NINTENDO_WII".to_owned()), [gamecube]).unwrap();

        let mut digital = port(0);
        digital
            .overrides
            .insert(key!(button.a), Some(key!(trigger.right)));
        assert!(matches!(
            hub_with([digital]),
            Err(InputConfigurationError::InvalidBinding(_))
        ));
    }

    #[test]
    fn layouts_are_serializable_for_frontend_discovery() {
        let layouts = hub().layouts();
        let json = serde_json::to_value(layouts).unwrap();
        assert!(
            json.as_array()
                .unwrap()
                .iter()
                .any(|layout| layout["id"] == "WIIU_DRC_CONTROLLER")
        );
    }

    #[test]
    fn duplicate_ports_are_rejected_instead_of_silently_overwriting() {
        let error = rejected(hub_with([port(0), port(0)]));
        assert_eq!(
            error,
            InputConfigurationError::DuplicatePort(VirtualPort::new("controller", 0))
        );
    }

    #[test]
    fn two_ports_may_not_drive_one_virtual_pad() {
        let mut second = port(1);
        second.slot = Some(0);
        assert_eq!(
            rejected(hub_with([port(0), second])),
            InputConfigurationError::DuplicateSlot(0)
        );
    }

    #[test]
    fn a_port_is_checked_against_the_machine_that_declares_it() {
        // An Xbox has four controller ports and no other group.
        let mut beyond = port(4);
        beyond.slot = Some(4);
        assert_eq!(
            rejected(hub_with([beyond])),
            InputConfigurationError::PortOutOfRange {
                port: VirtualPort::new("controller", 4),
                ports: 4,
            }
        );

        let mut elsewhere = port(0);
        elsewhere.port = VirtualPort::new("gamepad", 0);
        assert_eq!(
            rejected(hub_with([elsewhere])),
            InputConfigurationError::UnknownGroup {
                machine: MACHINE.to_owned(),
                group: "gamepad".to_owned(),
            }
        );
    }

    #[test]
    fn a_group_only_accepts_the_layouts_it_declares() {
        // The layout exists — it is simply not something an Xbox accepts. This
        // is the check a flat port index could not make at all.
        let mut wrong = port(0);
        wrong.layout = "WIIU_DRC_CONTROLLER".to_owned();
        assert_eq!(
            rejected(hub_with([wrong])),
            InputConfigurationError::LayoutNotAccepted {
                port: VirtualPort::new("controller", 0),
                layout: "WIIU_DRC_CONTROLLER".to_owned(),
            }
        );
    }

    fn wiiu(
        ports: impl IntoIterator<Item = PortConfiguration>,
    ) -> Result<ControllerConfiguration, InputConfigurationError> {
        ControllerConfiguration::new(Some("NINTENDO_WIIU".to_owned()), ports)
    }

    fn wiiu_port(group: &str, index: u8, layout: &str, slot: Option<u8>) -> PortConfiguration {
        PortConfiguration {
            port: VirtualPort::new(group, index),
            layout: layout.to_owned(),
            slot,
            force_mouse: false,
            overrides: ControllerBindings::default(),
        }
    }

    #[test]
    fn the_wii_u_groups_stay_apart() {
        // The case the flat model could not express: two device groups on one
        // machine, with disjoint input sets and different port counts.
        let hub = wiiu([
            wiiu_port("gamepad", 0, "WIIU_DRC_CONTROLLER", Some(0)),
            wiiu_port("controller", 0, "WIIU_PRO_CONTROLLER", Some(1)),
        ])
        .unwrap();
        assert_eq!(hub.snapshot().ports.len(), 2);

        // The GamePad does not fit a controller port, and there is only one of it.
        assert!(matches!(
            wiiu([wiiu_port("controller", 1, "WIIU_DRC_CONTROLLER", Some(0))]),
            Err(InputConfigurationError::LayoutNotAccepted { .. })
        ));
        assert!(matches!(
            wiiu([wiiu_port("gamepad", 1, "WIIU_DRC_CONTROLLER", Some(0))]),
            Err(InputConfigurationError::PortOutOfRange { .. })
        ));
    }

    #[test]
    fn an_accessory_group_hosts_no_controller() {
        assert!(matches!(
            wiiu([wiiu_port("usb", 0, "WIIU_PRO_CONTROLLER", None)]),
            Err(InputConfigurationError::AccessoryGroup { .. })
        ));
    }

    #[test]
    fn ports_without_a_machine_are_rejected() {
        assert_eq!(
            rejected(ControllerConfiguration::new(None, [port(0)])),
            InputConfigurationError::NoMachine
        );
        // An empty topology needs no machine, which is what a host holds before
        // a game is attached.
        assert!(ControllerConfiguration::new(None, []).is_ok());
    }

    #[test]
    fn a_port_round_trips_through_its_wire_form() {
        let port = VirtualPort::new("controller", 3);
        assert_eq!(port.to_string(), "controller.3");
        assert_eq!("controller.3".parse::<VirtualPort>().unwrap(), port);
        assert_eq!(
            serde_json::to_value(&port).unwrap(),
            serde_json::json!("controller.3")
        );
        for bad in ["controller", "controller.", ".0", "controller.x", ""] {
            assert!(bad.parse::<VirtualPort>().is_err(), "{bad}");
        }
    }

    #[test]
    fn replacing_the_topology_changes_the_machine_with_it() {
        let snapshot = hub()
            .apply(InputTransaction {
                expected_revision: 0,
                operations: vec![InputOperation::ReplacePorts {
                    machine: "NINTENDO_WIIU".to_owned(),
                    ports: vec![wiiu_port("gamepad", 0, "WIIU_DRC_CONTROLLER", Some(0))],
                }],
            })
            .unwrap();
        assert_eq!(snapshot.machine.as_deref(), Some("NINTENDO_WIIU"));
        assert_eq!(snapshot.ports[0].layout, "WIIU_DRC_CONTROLLER");
    }
}
