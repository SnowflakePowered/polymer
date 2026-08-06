//! Revisioned, API-neutral configuration for physical sources and virtual ports.
//!
//! This is the control plane for input. Capture providers and virtual-device
//! sinks remain data-plane implementations: they consume snapshots produced
//! here, but neither owns user configuration.

use crate::layout::{BindingKey, InputLayout, LayoutRegistry, SdlAxis, SdlButton};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    sync::Arc,
};

/// A stable emulator-facing controller port.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct VirtualPort(pub u8);

/// A source selector persisted independently from a particular SDL connection.
///
/// An SDL enumeration index is retained only as a compatibility selector.
/// New user configuration should prefer a GUID plus the duplicate ordinal, or
/// an exact connection ID supplied by a device snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SourceSelector {
    KeyboardMouse,
    SdlIndex {
        index: usize,
    },
    SdlName {
        name: String,
    },
    SdlGuid {
        guid: String,
        #[serde(default)]
        duplicate_ordinal: u16,
    },
    Device {
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
    pub source: Option<SourceSelector>,
    #[serde(default)]
    pub overrides: HashMap<BindingKey, BindingOverride>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputConfiguration {
    pub revision: u64,
    pub ports: Vec<PortConfiguration>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InputOperation {
    /// Replaces the complete port topology. Hosts use this when attaching a
    /// game whose declared controller ports differ from the previous game.
    ReplacePorts {
        ports: Vec<PortConfiguration>,
    },
    ConfigurePort {
        port: VirtualPort,
        layout: String,
        source: Option<SourceSelector>,
    },
    SetSource {
        port: VirtualPort,
        source: Option<SourceSelector>,
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
    registry: LayoutRegistry,
    state: RwLock<HubState>,
}

#[derive(Clone)]
struct HubState {
    revision: u64,
    ports: BTreeMap<VirtualPort, PortConfiguration>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputConfigurationError {
    RevisionConflict { expected: u64, actual: u64 },
    DuplicatePort(VirtualPort),
    UnknownLayout(String),
    UnknownPort(VirtualPort),
    UnknownTarget { layout: String, target: BindingKey },
    InvalidBinding(String),
}

impl fmt::Display for InputConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RevisionConflict { expected, actual } => write!(
                formatter,
                "input configuration revision conflict: expected {expected}, actual {actual}"
            ),
            Self::DuplicatePort(port) => write!(formatter, "duplicate virtual port {}", port.0),
            Self::UnknownLayout(layout) => write!(formatter, "unknown input layout {layout}"),
            Self::UnknownPort(port) => write!(formatter, "unknown virtual port {}", port.0),
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
        registry: LayoutRegistry,
        ports: impl IntoIterator<Item = PortConfiguration>,
    ) -> Result<Self, InputConfigurationError> {
        let ports = collect_ports(ports)?;
        validate_ports(&registry, &ports)?;
        Ok(Self {
            inner: Arc::new(HubInner {
                registry,
                state: RwLock::new(HubState { revision: 0, ports }),
            }),
        })
    }

    pub fn snapshot(&self) -> InputConfiguration {
        let state = self.inner.state.read();
        InputConfiguration {
            revision: state.revision,
            ports: state.ports.values().cloned().collect(),
        }
    }

    pub fn layouts(&self) -> Vec<InputLayout> {
        let mut layouts = self.inner.registry.layouts().cloned().collect::<Vec<_>>();
        layouts.sort_by(|left, right| left.id.cmp(&right.id));
        layouts
    }

    /// Applies all operations or none of them.
    pub fn apply(
        &self,
        transaction: InputTransaction,
    ) -> Result<InputConfiguration, InputConfigurationError> {
        let mut state = self.inner.state.write();
        if transaction.expected_revision != state.revision {
            return Err(InputConfigurationError::RevisionConflict {
                expected: transaction.expected_revision,
                actual: state.revision,
            });
        }
        let mut next = state.clone();
        for operation in transaction.operations {
            apply_operation(&mut next.ports, operation)?;
        }
        validate_ports(&self.inner.registry, &next.ports)?;
        next.revision = next.revision.wrapping_add(1);
        *state = next;
        drop(state);
        Ok(self.snapshot())
    }
}

fn apply_operation(
    ports: &mut BTreeMap<VirtualPort, PortConfiguration>,
    operation: InputOperation,
) -> Result<(), InputConfigurationError> {
    match operation {
        InputOperation::ReplacePorts { ports: replacement } => {
            *ports = collect_ports(replacement)?;
        }
        InputOperation::ConfigurePort {
            port,
            layout,
            source,
        } => {
            ports.insert(
                port,
                PortConfiguration {
                    port,
                    layout,
                    source,
                    overrides: HashMap::new(),
                },
            );
        }
        InputOperation::SetSource { port, source } => {
            port_mut(ports, port)?.source = source;
        }
        InputOperation::SetLayout { port, layout } => {
            let port = port_mut(ports, port)?;
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
            port_mut(ports, port)?.overrides.insert(target, binding);
        }
        InputOperation::ResetTarget { port, target } => {
            port_mut(ports, port)?.overrides.remove(&target);
        }
        InputOperation::ResetPortOverrides { port } => {
            port_mut(ports, port)?.overrides.clear();
        }
        InputOperation::RemovePort { port } => {
            if ports.remove(&port).is_none() {
                return Err(InputConfigurationError::UnknownPort(port));
            }
        }
    }
    Ok(())
}

fn collect_ports(
    ports: impl IntoIterator<Item = PortConfiguration>,
) -> Result<BTreeMap<VirtualPort, PortConfiguration>, InputConfigurationError> {
    let mut collected = BTreeMap::new();
    for port in ports {
        let id = port.port;
        if collected.insert(id, port).is_some() {
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

fn validate_ports(
    registry: &LayoutRegistry,
    ports: &BTreeMap<VirtualPort, PortConfiguration>,
) -> Result<(), InputConfigurationError> {
    for port in ports.values() {
        let layout = registry
            .layout(&port.layout)
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

    fn key(text: &str) -> BindingKey {
        text.parse().expect("test binding key")
    }
    use crate::layout::LayoutRegistry;

    fn hub() -> InputHub {
        InputHub::new(
            LayoutRegistry::embedded().unwrap(),
            [PortConfiguration {
                port: VirtualPort(0),
                layout: "XBOX_CONTROLLER".to_owned(),
                source: Some(SourceSelector::SdlIndex { index: 0 }),
                overrides: HashMap::new(),
            }],
        )
        .unwrap()
    }

    #[test]
    fn transaction_is_atomic_and_revisioned() {
        let hub = hub();
        let snapshot = hub
            .apply(InputTransaction {
                expected_revision: 0,
                operations: vec![
                    InputOperation::SetOverride {
                        port: VirtualPort(0),
                        target: key("button.a"),
                        binding: BindingOverride::Button {
                            source: SourceControl::GamepadButton {
                                button: SdlButton::East,
                            },
                        },
                    },
                    InputOperation::SetSource {
                        port: VirtualPort(0),
                        source: Some(SourceSelector::KeyboardMouse),
                    },
                ],
            })
            .unwrap();
        assert_eq!(snapshot.revision, 1);
        assert_eq!(
            snapshot.ports[0].source,
            Some(SourceSelector::KeyboardMouse)
        );
        assert!(snapshot.ports[0].overrides.contains_key(&key("button.a")));
    }

    #[test]
    fn stale_transaction_does_not_modify_configuration() {
        let hub = hub();
        hub.apply(InputTransaction {
            expected_revision: 0,
            operations: vec![InputOperation::ResetPortOverrides {
                port: VirtualPort(0),
            }],
        })
        .unwrap();
        let error = hub
            .apply(InputTransaction {
                expected_revision: 0,
                operations: vec![InputOperation::RemovePort {
                    port: VirtualPort(0),
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
                    InputOperation::SetSource {
                        port: VirtualPort(0),
                        source: None,
                    },
                    InputOperation::SetOverride {
                        port: VirtualPort(0),
                        target: key("button.z"),
                        binding: BindingOverride::Disabled,
                    },
                ],
            })
            .unwrap_err();
        assert!(matches!(
            error,
            InputConfigurationError::UnknownTarget { .. }
        ));
        assert!(hub.snapshot().ports[0].source.is_some());
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
        let registry = LayoutRegistry::embedded().unwrap();
        let port = PortConfiguration {
            port: VirtualPort(0),
            layout: "XBOX_CONTROLLER".to_owned(),
            source: None,
            overrides: HashMap::new(),
        };
        assert!(matches!(
            InputHub::new(registry, [port.clone(), port]),
            Err(InputConfigurationError::DuplicatePort(VirtualPort(0)))
        ));
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
