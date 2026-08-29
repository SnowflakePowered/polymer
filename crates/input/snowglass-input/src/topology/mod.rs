//! Canonical controller topology shared by launch, host, and plugin layers.

use crate::{hub::SourceSelector, layout::ProjectionMode};
use consolespec::input::BindingKey;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use std::{collections::HashMap, str::FromStr};

/// An emulator-owned controller port.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerTarget {
    pub group: String,
    pub port: usize,
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
            .collect::<Result<Vec<_>, String>>()
            .map(Self::new)
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
        bindings
            .into_iter()
            .map(|(target, source)| {
                let target = BindingKey::from_str(&target).map_err(D::Error::custom)?;
                let source = if source == "none" {
                    None
                } else {
                    Some(BindingKey::from_str(&source).map_err(D::Error::custom)?)
                };
                Ok((target, source))
            })
            .collect::<Result<Vec<_>, D::Error>>()
            .map(Self::new)
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
    pub fn mapping_for_gamepad(&self, gamepad: u8) -> Option<&ControllerMapping> {
        self.mappings
            .iter()
            .find(|mapping| mapping.gamepad == gamepad)
    }

    pub fn port_assignments(&self) -> Result<snowglass_config::ast::PortAssignments, String> {
        let mut assignments = serde_json::Map::new();
        let mut gamepads = std::collections::HashSet::new();
        for mapping in &self.mappings {
            if !gamepads.insert(mapping.gamepad) {
                return Err(format!(
                    "virtual gamepad {} is assigned more than once",
                    mapping.gamepad
                ));
            }
            let layouts = assignments
                .entry(mapping.target.group.clone())
                .or_insert_with(|| serde_json::Value::Array(Vec::new()))
                .as_array_mut()
                .expect("controller assignment groups are arrays");
            if layouts.len() <= mapping.target.port {
                layouts.resize(mapping.target.port + 1, serde_json::Value::Null);
            }
            if !layouts[mapping.target.port].is_null() {
                return Err(format!(
                    "controller target {}:{} is assigned more than once",
                    mapping.target.group, mapping.target.port
                ));
            }
            layouts[mapping.target.port] = mapping.layout.clone().into();
        }
        if gamepads
            .iter()
            .max()
            .is_some_and(|last| usize::from(*last) + 1 != gamepads.len())
        {
            return Err("controller topology has a hole in its virtual gamepads".to_owned());
        }
        if let Some((group, port)) = assignments.iter().find_map(|(group, layouts)| {
            layouts
                .as_array()
                .and_then(|layouts| layouts.iter().position(serde_json::Value::is_null))
                .map(|port| (group, port))
        }) {
            return Err(format!("controller topology omits target {group}:{port}"));
        }
        serde_json::from_value(serde_json::Value::Object(assignments))
            .map_err(|error| error.to_string())
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
