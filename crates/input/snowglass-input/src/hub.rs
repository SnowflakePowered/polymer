//! Revisioned, API-neutral configuration for physical sources and virtual ports.
//!
//! This is the control plane for input. Capture providers and virtual-device
//! sinks remain data-plane implementations: they consume snapshots produced
//! here, but neither owns user configuration.

use crate::layout::{InputLayout, SdlAxis, SdlButton};
use consolespec::MachineSpec;
use consolespec::input::BindingKey;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    sync::Arc,
};

/// A stable emulator-facing controller port: a machinespec input group and an
/// index within it.
///
/// A machine's ports are not one flat run. A Wii U has one `gamepad` port and
/// eight `controller` ports, which accept disjoint sets of devices; a Wii has
/// four `wii` ports *and* four GameCube `controller` ports on a separate bus.
/// Naming the group is what lets [`InputHub`] check a port against the machine
/// that declares it — and it is deliberately **not** the virtual pad index, which
/// is [`PortConfiguration::slot`]: a port in passthrough consumes an emulator
/// port but no pad, so the two runs skew.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VirtualPort {
    /// A machinespec `[input]` group name, e.g. `controller`, `gamepad`, `wii`.
    pub group: String,
    /// Zero-based index within the group.
    pub index: u8,
}

impl VirtualPort {
    pub fn new(group: impl Into<String>, index: u8) -> Self {
        Self {
            group: group.into(),
            index,
        }
    }
}

impl fmt::Display for VirtualPort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.group, self.index)
    }
}

impl std::str::FromStr for VirtualPort {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (group, index) = text
            .rsplit_once('.')
            .ok_or_else(|| format!("`{text}` is not a port; expected `<group>.<index>`"))?;
        if group.is_empty() {
            return Err(format!("`{text}` names no group"));
        }
        let index = index
            .parse()
            .map_err(|_| format!("`{text}` has no port index"))?;
        Ok(Self::new(group, index))
    }
}

/// The wire form is `"controller.0"`, so the frontend can compare ports with
/// `===` and use one as an object key, as it did when a port was a bare number.
impl Serialize for VirtualPort {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for VirtualPort {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = std::borrow::Cow::<'de, str>::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

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

/// A source selector persisted independently from a particular SDL connection.
///
/// Provider implementation details such as SDL enumeration indices, names,
/// and GUIDs never enter persisted configuration. A physical device is named
/// only by its opaque, provider-owned durable ID.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SourceSelector {
    KeyboardMouse,
    Device {
        #[serde(rename = "deviceId", alias = "device_id")]
        device_id: String,
    },
}

/// A physical control which can feed an element target.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SourceControl {
    GamepadButton { button: SdlButton },
    GamepadAxis { axis: SdlAxis },
    Key { code: String },
    MouseButton { button: u8 },
    MouseDelta { axis: PointerAxis },
    MousePosition { axis: PointerAxis },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PointerAxis {
    X,
    Y,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AxisTransform {
    #[serde(default)]
    pub invert: bool,
    #[serde(default = "one")]
    pub scale: f32,
    #[serde(default)]
    pub dead_zone: f32,
}

impl Default for AxisTransform {
    fn default() -> Self {
        Self {
            invert: false,
            scale: 1.0,
            dead_zone: 0.0,
        }
    }
}

const fn one() -> f32 {
    1.0
}

/// A user override for one input element.
///
/// The map key is always a binding key — `button.a`, `analog.stick.left.x+` —
/// as the layout spells it, not the element id. `Disabled` is deliberately
/// distinct from removing the entry, which restores the inherited mapping.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BindingOverride {
    Button {
        source: SourceControl,
    },
    Axis {
        source: SourceControl,
        #[serde(default)]
        transform: AxisTransform,
    },
    Disabled,
}

impl BindingOverride {
    /// Projects the subset supported by today's SDL-to-SDL data plane.
    pub fn sdl_binding(&self) -> Option<crate::layout::InputBinding> {
        match self {
            Self::Button {
                source: SourceControl::GamepadButton { button },
            } => Some(crate::layout::InputBinding::Button { button: *button }),
            Self::Axis {
                source: SourceControl::GamepadAxis { axis },
                transform,
            } => Some(crate::layout::InputBinding::Axis {
                axis: *axis,
                invert: transform.invert,
            }),
            Self::Disabled => Some(crate::layout::InputBinding::Disabled { disabled: true }),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortConfiguration {
    pub port: VirtualPort,
    pub layout: String,
    /// Which virtual gamepad this port drives, if any.
    ///
    /// Separate from [`VirtualPort`] on purpose. Ports are numbered per group by
    /// the machine; virtual pads are numbered by the SDL provider, densely, over
    /// only the ports that present one. A port in passthrough or left empty
    /// consumes the former and not the latter, so `controller.3` is routinely
    /// slot 2. Conflating them is what §6.3 of the config design calls port
    /// index skew, and it silently binds player 3's config to player 2's pad.
    pub slot: Option<u8>,
    /// Prefer pointer input from the host mouse even when the physical source
    /// is a controller with its own pointer-capable controls.
    #[serde(default)]
    pub force_mouse: bool,
    #[serde(default)]
    pub overrides: HashMap<BindingKey, BindingOverride>,
}

/// A durable physical-source binding for one virtual controller index.
///
/// This is deliberately separate from [`PortConfiguration`]. A game decides
/// which virtual controller feeds each emulator port; the native controller
/// manager decides which physical source feeds that virtual controller, and
/// that decision survives provider disconnects.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceAssignment {
    pub index: u8,
    pub source: SourceSelector,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputConfiguration {
    pub revision: u64,
    /// The machinespec this topology's ports belong to. `None` before any game
    /// is attached, when there are no ports at all.
    pub machine: Option<String>,
    pub ports: Vec<PortConfiguration>,
    pub assignments: Vec<SourceAssignment>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InputOperation {
    /// Replaces the complete port topology. Hosts use this when attaching a
    /// game whose declared controller ports differ from the previous game.
    ///
    /// The machine comes with it: a port only means something against the
    /// machinespec that declares its group, so changing the topology and
    /// changing the machine is one operation, not two.
    ReplacePorts {
        machine: String,
        ports: Vec<PortConfiguration>,
    },
    ConfigurePort {
        port: VirtualPort,
        layout: String,
        #[serde(default)]
        slot: Option<u8>,
    },
    SetAssignment {
        index: u8,
        source: Option<SourceSelector>,
    },
    ReplaceAssignments {
        assignments: Vec<SourceAssignment>,
    },
    SetLayout {
        port: VirtualPort,
        layout: String,
    },
    SetOverride {
        port: VirtualPort,
        target: BindingKey,
        binding: BindingOverride,
    },
    ResetTarget {
        port: VirtualPort,
        target: BindingKey,
    },
    ResetPortOverrides {
        port: VirtualPort,
    },
    RemovePort {
        port: VirtualPort,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputTransaction {
    pub expected_revision: u64,
    pub operations: Vec<InputOperation>,
}

#[derive(Clone)]
pub struct InputHub {
    inner: Arc<HubInner>,
}

struct HubInner {
    state: RwLock<HubState>,
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
                        overrides: HashMap::new(),
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
                validate_binding(&binding)?;
                port_mut(&mut self.ports, port)?
                    .overrides
                    .insert(target, binding);
            }
            InputOperation::ResetTarget { port, target } => {
                port_mut(&mut self.ports, port)?.overrides.remove(&target);
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

impl InputHub {
    pub fn new(
        machine: Option<String>,
        ports: impl IntoIterator<Item = PortConfiguration>,
    ) -> Result<Self, InputConfigurationError> {
        let ports = collect_ports(ports)?;
        validate_ports(machine.as_deref(), &ports)?;
        Ok(Self {
            inner: Arc::new(HubInner {
                state: RwLock::new(HubState {
                    revision: 0,
                    machine,
                    ports,
                    assignments: BTreeMap::new(),
                }),
            }),
        })
    }

    pub fn snapshot(&self) -> InputConfiguration {
        let state = self.inner.state.read();
        state.configuration()
    }

    /// The attached machine's consolespec entry, for a caller that needs to ask
    /// it something the hub does not expose — which group accepts a layout, say.
    pub fn machine(&self) -> Option<MachineSpec> {
        machine_spec(self.inner.state.read().machine.as_deref())
    }

    /// The input groups of the attached machine, in declaration order. Empty
    /// before a machine is declared.
    pub fn groups(&self) -> Vec<InputGroup> {
        let state = self.inner.state.read();
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
        let state = self.inner.state.read();
        state.prepare(transaction).map(|next| next.configuration())
    }

    /// Applies all operations or none of them.
    pub fn apply(
        &self,
        transaction: InputTransaction,
    ) -> Result<InputConfiguration, InputConfigurationError> {
        let mut state = self.inner.state.write();
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
        for (&target, binding) in &port.overrides {
            if !layout.has(target) {
                return Err(InputConfigurationError::UnknownTarget {
                    layout: layout.id.clone(),
                    target,
                });
            }
            if matches!(binding, BindingOverride::Axis { .. }) && !target.is_analog() {
                return Err(InputConfigurationError::InvalidBinding(format!(
                    "element {target} does not accept an axis binding"
                )));
            }
            validate_binding(binding)?;
        }
    }
    Ok(())
}

fn validate_binding(binding: &BindingOverride) -> Result<(), InputConfigurationError> {
    let transform = match binding {
        BindingOverride::Axis { transform, .. } => Some(transform),
        BindingOverride::Button { .. } | BindingOverride::Disabled => None,
    };
    if let Some(transform) = transform {
        if !transform.scale.is_finite() || transform.scale < 0.0 {
            return Err(InputConfigurationError::InvalidBinding(
                "axis scale must be finite and non-negative".to_owned(),
            ));
        }
        if !transform.dead_zone.is_finite() || !(0.0..1.0).contains(&transform.dead_zone) {
            return Err(InputConfigurationError::InvalidBinding(
                "axis deadZone must be finite and in 0..1".to_owned(),
            ));
        }
    }
    match binding {
        BindingOverride::Button {
            source: SourceControl::Key { code },
        } if code.trim().is_empty() => Err(InputConfigurationError::InvalidBinding(
            "key binding code must not be empty".to_owned(),
        )),
        _ => Ok(()),
    }
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
            overrides: HashMap::new(),
        }
    }

    fn hub_with(
        ports: impl IntoIterator<Item = PortConfiguration>,
    ) -> Result<InputHub, InputConfigurationError> {
        InputHub::new(Some(MACHINE.to_owned()), ports)
    }

    fn hub() -> InputHub {
        hub_with([port(0)]).unwrap()
    }

    /// `InputHub` is not `Debug` — it owns a lock — so the error path takes the
    /// error rather than unwrapping the success.
    fn rejected(result: Result<InputHub, InputConfigurationError>) -> InputConfigurationError {
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
                        binding: BindingOverride::Button {
                            source: SourceControl::GamepadButton {
                                button: SdlButton::East,
                            },
                        },
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
        assert!(snapshot.ports[0].overrides.contains_key(&key!(button.a)));
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
                        binding: BindingOverride::Disabled,
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
    fn generic_bindings_report_whether_the_sdl_projection_supports_them() {
        let key = BindingOverride::Button {
            source: SourceControl::Key {
                code: "KeyX".to_owned(),
            },
        };
        assert_eq!(key.sdl_binding(), None);
        let axis = BindingOverride::Axis {
            source: SourceControl::GamepadAxis {
                axis: SdlAxis::RightX,
            },
            transform: AxisTransform {
                invert: true,
                ..AxisTransform::default()
            },
        };
        assert!(matches!(
            axis.sdl_binding(),
            Some(crate::layout::InputBinding::Axis {
                axis: SdlAxis::RightX,
                invert: true
            })
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
    ) -> Result<InputHub, InputConfigurationError> {
        InputHub::new(Some("NINTENDO_WIIU".to_owned()), ports)
    }

    fn wiiu_port(group: &str, index: u8, layout: &str, slot: Option<u8>) -> PortConfiguration {
        PortConfiguration {
            port: VirtualPort::new(group, index),
            layout: layout.to_owned(),
            slot,
            force_mouse: false,
            overrides: HashMap::new(),
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
            rejected(InputHub::new(None, [port(0)])),
            InputConfigurationError::NoMachine
        );
        // An empty topology needs no machine, which is what a host holds before
        // a game is attached.
        assert!(InputHub::new(None, []).is_ok());
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

    #[test]
    fn mouse_axis_binding_uses_the_javascript_wire_shape() {
        let binding: BindingOverride = serde_json::from_value(serde_json::json!({
            "kind": "axis",
            "source": {
                "kind": "mouseDelta",
                "axis": "x"
            },
            "transform": {
                "scale": 1.5,
                "deadZone": 0.02,
                "invert": true
            }
        }))
        .unwrap();
        assert_eq!(
            binding,
            BindingOverride::Axis {
                source: SourceControl::MouseDelta {
                    axis: PointerAxis::X
                },
                transform: AxisTransform {
                    scale: 1.5,
                    dead_zone: 0.02,
                    invert: true,
                },
            }
        );
    }
}
