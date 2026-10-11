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
        let priority = self
            .machine
            .as_deref()
            .and_then(|machine| MachineSpec::try_from(machine).ok())
            .map(|machine| {
                machine
                    .input_groups()
                    .map(|group| group.name())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut ports = self.ports.values().cloned().collect::<Vec<_>>();
        ports.sort_by_key(|port| {
            let rank = priority.iter().position(|group| *group == port.port.group);
            (rank.unwrap_or(priority.len()), port.port.index)
        });
        InputConfiguration {
            revision: self.revision,
            machine: self.machine.clone(),
            ports,
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
