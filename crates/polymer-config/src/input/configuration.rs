use consolespec::input::BindingKey;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use std::{collections::HashMap, fmt, str::FromStr};

/// How physical controls are projected onto a controller layout.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectionMode {
    /// Match controls by their semantic names.
    Name,
    /// Match controls by their physical positions.
    #[default]
    Position,
    /// Use only the explicitly configured bindings.
    Custom,
}

/// Serializable overrides from layout elements to physical controller controls.
///
/// A `None` source explicitly disables the target rather than falling back to
/// the layout's default projection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ControllerBindings(HashMap<BindingKey, Option<BindingKey>>);

impl ControllerBindings {
    pub fn new(bindings: impl IntoIterator<Item = (BindingKey, Option<BindingKey>)>) -> Self {
        Self(bindings.into_iter().collect())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&BindingKey, Option<&BindingKey>)> {
        self.0
            .iter()
            .map(|(target, source)| (target, source.as_ref()))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, target: BindingKey) -> bool {
        self.0.contains_key(&target)
    }

    pub fn insert(&mut self, target: BindingKey, source: Option<BindingKey>) {
        self.0.insert(target, source);
    }

    pub fn remove(&mut self, target: BindingKey) {
        self.0.remove(&target);
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn try_from_strings(
        bindings: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, String> {
        bindings
            .into_iter()
            .map(|(target, source)| {
                let target = BindingKey::from_str(&target).map_err(|error| error.to_string())?;
                let source = match source.as_str() {
                    "none" => None,
                    _ => Some(BindingKey::from_str(&source).map_err(|error| error.to_string())?),
                };
                Ok((target, source))
            })
            .collect::<Result<HashMap<_, _>, String>>()
            .map(Self)
    }
}

impl Serialize for ControllerBindings {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.iter()
            .map(|(target, source)| {
                (
                    target.to_string(),
                    source.map_or_else(|| "none".to_owned(), ToString::to_string),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ControllerBindings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bindings = HashMap::<String, String>::deserialize(deserializer)?;
        Self::try_from_strings(bindings).map_err(D::Error::custom)
    }
}

/// A port declared by a machine's input topology.
///
/// The wire form is `"controller.0"`, which keeps the group and its zero-based
/// index together as one stable configuration key.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VirtualPort {
    /// A machinespec input group name, such as `controller`, `gamepad`, or `wii`.
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

impl FromStr for VirtualPort {
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

impl Serialize for VirtualPort {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for VirtualPort {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = std::borrow::Cow::<'de, str>::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// A source selector persisted independently from a backend connection.
///
/// Enumeration indices, display names, and backend-specific identifiers do not
/// enter configuration. Physical devices are selected by an opaque durable ID.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SourceSelector {
    KeyboardMouse,
    Device {
        #[serde(rename = "deviceId", alias = "device_id")]
        device_id: String,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortConfiguration {
    pub port: VirtualPort,
    pub layout: String,
    /// The virtual gamepad this port drives, if it presents one.
    ///
    /// This is separate from [`VirtualPort::index`]: ports are numbered within
    /// machine groups, while virtual gamepads are numbered densely across only
    /// the ports that present one.
    pub slot: Option<u8>,
    /// Prefer host mouse input over pointer-capable controller input.
    #[serde(default)]
    pub force_mouse: bool,
    /// Per-target changes layered over the selected projection mode.
    #[serde(default)]
    pub overrides: ControllerBindings,
}

/// A durable physical-source binding for one virtual controller index.
///
/// This is deliberately separate from [`PortConfiguration`]. A game determines
/// which virtual controller feeds each emulator port; this assignment determines
/// which physical source feeds that virtual controller and survives disconnects.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceAssignment {
    pub index: u8,
    pub source: SourceSelector,
}

/// The complete serializable input configuration snapshot.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputConfiguration {
    pub revision: u64,
    /// The machinespec whose topology declares `ports`, or `None` before a game
    /// is attached and no ports exist.
    pub machine: Option<String>,
    pub ports: Vec<PortConfiguration>,
    pub assignments: Vec<SourceAssignment>,
}

impl InputConfiguration {
    pub fn source_for(&self, index: u8) -> Option<&SourceSelector> {
        self.assignments
            .iter()
            .find(|assignment| assignment.index == index)
            .map(|assignment| &assignment.source)
    }
}

/// An atomic edit within an [`InputTransaction`].
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InputOperation {
    /// Replace the complete port topology when attaching a different machine.
    ///
    /// The machine travels with the ports because group names and ranges only
    /// have meaning against the machinespec that declares them.
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
        binding: Option<BindingKey>,
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

/// A revision-checked batch of input configuration changes.
///
/// Consumers prepare and validate every operation against one snapshot, then
/// commit the resulting configuration atomically if `expected_revision` still
/// matches. This prevents stale frontends from partially overwriting newer input
/// state.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputTransaction {
    pub expected_revision: u64,
    pub operations: Vec<InputOperation>,
}
