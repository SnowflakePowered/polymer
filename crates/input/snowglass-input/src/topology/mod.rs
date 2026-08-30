//! Canonical controller topology shared by launch, host, and plugin layers.

use crate::{
    hub::{
        BindingOverride, InputHub, InputOperation, InputTransaction, PortConfiguration,
        SourceAssignment, SourceSelector, VirtualPort,
    },
    layout::{InputBinding, InputLayout, ProjectionMode},
    source::InputSourceDescriptor,
};
use consolespec::input::BindingKey;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    str::FromStr,
};

/// An emulator-owned controller port.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerTarget {
    pub group: String,
    pub port: usize,
}

/// One emulator-owned port and the layouts it accepts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControllerPort {
    pub group: String,
    pub index: usize,
    pub layouts: Vec<String>,
}

/// A typed logical-to-physical binding map.
///
/// JSON retains the plugin-facing dotted expression vocabulary. `null` is not
/// accepted: the string `none` is the sole representation of a disabled target.
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

    pub fn defaults(layout: &InputLayout) -> Self {
        Self::new(
            layout
                .default_gamepad_bindings()
                .map(|(target, source)| (target, Some(source))),
        )
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

    pub fn from_overrides(
        layout: &InputLayout,
        overrides: &HashMap<BindingKey, BindingOverride>,
    ) -> Result<Self, String> {
        overrides
            .iter()
            .map(|(target, binding)| {
                let binding = binding.sdl_binding().ok_or_else(|| {
                    format!("input override for {target} is not a physical gamepad expression")
                })?;
                let source = match binding {
                    InputBinding::Disabled { .. } => None,
                    binding => Some(layout.custom_source(*target, &binding).ok_or_else(|| {
                        format!("input override for {target} has no canonical gamepad expression")
                    })?),
                };
                Ok((*target, source))
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
        let bindings = self
            .iter()
            .map(|(target, source)| {
                (
                    target.to_string(),
                    source.map_or_else(|| "none".to_owned(), ToString::to_string),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        bindings.serialize(serializer)
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

/// One stable virtual gamepad routed to an emulator port.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerMapping {
    pub target: ControllerTarget,
    pub gamepad: u8,
    pub source: Option<SourceSelector>,
    pub layout: String,
    pub connected: bool,
    /// Provider-owned identity metadata for the selected physical source.
    /// Plugins may use this to select an emulator-native input backend, but
    /// the opaque source selector remains the runtime routing authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<InputSourceDescriptor>,
    #[serde(default)]
    pub mouse: bool,
    #[serde(default)]
    pub projection_mode: ProjectionMode,
    #[serde(default)]
    pub mapping: ControllerBindings,
}

/// Complete emulator controller topology. Disconnected ports remain present so
/// layouts can change without reallocating virtual gamepads.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerTopology {
    pub mappings: Vec<ControllerMapping>,
}

impl ControllerTopology {
    /// Completes a partial selection with disconnected mappings for every port.
    pub fn complete(
        ports: Vec<ControllerPort>,
        selected: Vec<ControllerMapping>,
    ) -> Result<Self, String> {
        if ports.len() > usize::from(u8::MAX) + 1 {
            return Err("an emulator cannot expose more than 256 virtual gamepads".to_owned());
        }

        let declared = ports
            .iter()
            .map(|port| {
                (
                    ControllerTarget {
                        group: port.group.clone(),
                        port: port.index,
                    },
                    port,
                )
            })
            .collect::<HashMap<_, _>>();
        if declared.len() != ports.len() {
            return Err("an emulator declares a controller target more than once".to_owned());
        }

        let mut used_slots = HashSet::new();
        let mut selected_by_target = HashMap::new();
        for mapping in selected {
            let port = declared.get(&mapping.target).ok_or_else(|| {
                format!(
                    "unknown emulator port {}:{}",
                    mapping.target.group, mapping.target.port
                )
            })?;
            if !port.layouts.contains(&mapping.layout) {
                return Err(format!(
                    "layout '{}' is not supported by {}:{}",
                    mapping.layout, mapping.target.group, mapping.target.port
                ));
            }
            if usize::from(mapping.gamepad) >= ports.len() || !used_slots.insert(mapping.gamepad) {
                return Err(format!(
                    "virtual gamepad {} is invalid or duplicated",
                    mapping.gamepad
                ));
            }
            if selected_by_target
                .insert(mapping.target.clone(), mapping)
                .is_some()
            {
                return Err("a controller target was selected more than once".to_owned());
            }
        }

        let mut available_slots =
            (0..ports.len()).filter(|slot| !used_slots.contains(&(*slot as u8)));
        let mappings = ports
            .into_iter()
            .map(|port| {
                let target = ControllerTarget {
                    group: port.group,
                    port: port.index,
                };
                if let Some(mapping) = selected_by_target.remove(&target) {
                    return Ok(mapping);
                }
                let gamepad = available_slots
                    .next()
                    .expect("unselected ports have unused virtual gamepads")
                    as u8;
                let layout = port.layouts.into_iter().next().ok_or_else(|| {
                    format!(
                        "emulator port {}:{} declares no layouts",
                        target.group, target.port
                    )
                })?;
                Ok(ControllerMapping {
                    target,
                    gamepad,
                    source: None,
                    layout,
                    connected: false,
                    device: None,
                    mouse: false,
                    projection_mode: ProjectionMode::Position,
                    mapping: ControllerBindings::default(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let topology = Self { mappings };
        topology.validate()?;
        Ok(topology)
    }

    pub fn mapping_for_gamepad(&self, gamepad: u8) -> Option<&ControllerMapping> {
        self.mappings
            .iter()
            .find(|mapping| mapping.gamepad == gamepad)
    }

    pub fn validate(&self) -> Result<(), String> {
        let mut slots = HashSet::new();
        let mut targets = HashSet::new();
        for mapping in &self.mappings {
            if !slots.insert(mapping.gamepad) {
                return Err(format!(
                    "virtual gamepad {} is assigned more than once",
                    mapping.gamepad
                ));
            }
            if !targets.insert(&mapping.target) {
                return Err(format!(
                    "controller target {}:{} is assigned more than once",
                    mapping.target.group, mapping.target.port
                ));
            }
            let layout = InputLayout::get(&mapping.layout)
                .ok_or_else(|| format!("unknown input layout {}", mapping.layout))?;
            let mut binding_targets = HashSet::new();
            for (target, source) in mapping.mapping.iter() {
                if !layout.has(*target) {
                    return Err(format!(
                        "input layout {} has no element {target}",
                        layout.id
                    ));
                }
                binding_targets.insert(*target);
                if let Some(source) = source {
                    layout.custom_binding(*target, *source).ok_or_else(|| {
                        format!("input source '{source}' is not a physical gamepad expression")
                    })?;
                }
            }
            if mapping.projection_mode == ProjectionMode::Custom {
                let missing = layout
                    .custom_binding_targets()
                    .filter(|target| !binding_targets.contains(target))
                    .map(|target| target.to_string())
                    .collect::<Vec<_>>();
                if !missing.is_empty() {
                    return Err(format!(
                        "custom projection for {} must specify: {}",
                        layout.id,
                        missing.join(", ")
                    ));
                }
            }
        }
        if slots
            .iter()
            .max()
            .is_some_and(|last| usize::from(*last) + 1 != slots.len())
        {
            return Err("controller topology has a hole in its virtual gamepads".to_owned());
        }
        Ok(())
    }

    pub fn validate_for_machine(&self, machine: &str) -> Result<(), String> {
        let hub = InputHub::new(None, []).expect("an empty topology is valid");
        hub.preview(&self.input_transaction(machine, 0)?)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub fn input_transaction(
        &self,
        machine: &str,
        expected_revision: u64,
    ) -> Result<InputTransaction, String> {
        self.validate()?;
        let mut ports = Vec::with_capacity(self.mappings.len());
        let mut assignments = Vec::new();
        for mapping in &self.mappings {
            let layout = InputLayout::get(&mapping.layout)
                .ok_or_else(|| format!("unknown input layout {}", mapping.layout))?;
            let overrides =
                mapping
                    .mapping
                    .iter()
                    .map(|(target, source)| {
                        let binding = match source {
                        Some(source) => layout.custom_binding(*target, *source).ok_or_else(|| {
                            format!(
                                "input source '{source}' is not a physical gamepad expression"
                            )
                        })?,
                        None => InputBinding::Disabled { disabled: true },
                    };
                        Ok((*target, binding.into()))
                    })
                    .collect::<Result<HashMap<_, _>, String>>()?;
            let port = u8::try_from(mapping.target.port).map_err(|_| {
                format!(
                    "controller target {}:{} exceeds 255 ports",
                    mapping.target.group, mapping.target.port
                )
            })?;
            ports.push(PortConfiguration {
                port: VirtualPort::new(&mapping.target.group, port),
                layout: mapping.layout.clone(),
                slot: Some(mapping.gamepad),
                force_mouse: mapping.mouse,
                overrides,
            });
            if let Some(source) = &mapping.source {
                assignments.push(SourceAssignment {
                    index: mapping.gamepad,
                    source: source.clone(),
                });
            }
        }
        Ok(InputTransaction {
            expected_revision,
            operations: vec![
                InputOperation::ReplacePorts {
                    machine: machine.to_owned(),
                    ports,
                },
                InputOperation::ReplaceAssignments { assignments },
            ],
        })
    }

    pub fn port_assignments(&self) -> Result<snowglass_config::ast::PortAssignments, String> {
        self.validate()?;
        let mut assignments = BTreeMap::<String, BTreeMap<usize, String>>::new();
        for mapping in &self.mappings {
            assignments
                .entry(mapping.target.group.clone())
                .or_default()
                .insert(mapping.target.port, mapping.layout.clone());
        }
        assignments
            .into_iter()
            .map(|(group, layouts)| {
                let layouts = layouts
                    .into_iter()
                    .enumerate()
                    .map(|(expected, (port, layout))| {
                        (port == expected)
                            .then(|| layout.into_boxed_str())
                            .ok_or_else(|| {
                                format!("controller topology omits target {group}:{expected}")
                            })
                    })
                    .collect::<Result<Vec<_>, String>>()?
                    .into_boxed_slice();
                Ok((group.into_boxed_str(), layouts))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_reject_ambiguous_or_invalid_expressions() {
        let invalid = serde_json::from_value::<ControllerBindings>(serde_json::json!({
            "button.a": null,
        }));
        assert!(invalid.is_err());

        let invalid = serde_json::from_value::<ControllerBindings>(serde_json::json!({
            "not an input": "button.a",
        }));
        assert!(invalid.is_err());
    }

    #[test]
    fn port_assignments_reject_holes_and_duplicate_targets() {
        let mapping = |port| ControllerMapping {
            target: ControllerTarget {
                group: "controller".to_owned(),
                port,
            },
            gamepad: port as u8,
            source: None,
            layout: "XBOX_CONTROLLER".to_owned(),
            connected: false,
            device: None,
            mouse: false,
            projection_mode: ProjectionMode::Position,
            mapping: ControllerBindings::default(),
        };
        assert!(
            ControllerTopology {
                mappings: vec![mapping(1)],
            }
            .port_assignments()
            .is_err()
        );
        assert!(
            ControllerTopology {
                mappings: vec![mapping(0), mapping(0)],
            }
            .port_assignments()
            .is_err()
        );
        let mut duplicate_gamepad = mapping(1);
        duplicate_gamepad.gamepad = 0;
        assert!(
            ControllerTopology {
                mappings: vec![mapping(0), duplicate_gamepad],
            }
            .port_assignments()
            .is_err()
        );
        let mut gamepad_hole = mapping(0);
        gamepad_hole.gamepad = 1;
        assert!(
            ControllerTopology {
                mappings: vec![gamepad_hole],
            }
            .port_assignments()
            .is_err()
        );
    }
}
