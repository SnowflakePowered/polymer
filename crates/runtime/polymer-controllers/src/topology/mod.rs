//! Canonical controller topology shared by launch, host, and plugin layers.

mod wire;
pub use wire::*;

use crate::configuration::ControllerConfiguration;
use crate::layout::InputLayout;
use polymer_config::input::{
    ControllerBindings, InputOperation, InputTransaction, PortConfiguration, ProjectionMode,
    SourceAssignment, SourceSelector, VirtualPort,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

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
    pub pointer: PointerMode,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerDevice {
    pub id: String,
    pub name: String,
    pub profile: Option<String>,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub serial: Option<String>,
    pub role: String,
    pub capabilities: Vec<String>,
}

impl ControllerDevice {
    pub fn from_descriptor(descriptor: &polymer_input::DeviceDescriptor) -> Self {
        Self {
            id: descriptor.id.to_string(),
            name: descriptor.name.to_string(),
            profile: descriptor.profile.as_ref().map(ToString::to_string),
            vendor_id: descriptor.vendor_id,
            product_id: descriptor.product_id,
            serial: descriptor.serial.as_ref().map(ToString::to_string),
            role: format!("{:?}", descriptor.role),
            capabilities: Self::capabilities(descriptor.capabilities),
        }
    }

    fn capabilities(capabilities: polymer_input::Capabilities) -> Vec<String> {
        [
            (polymer_input::Capabilities::RUMBLE, "rumble"),
            (polymer_input::Capabilities::HD_RUMBLE, "hdRumble"),
            (polymer_input::Capabilities::PLAYER_LED, "playerLed"),
            (polymer_input::Capabilities::HOME_LED, "homeLed"),
            (polymer_input::Capabilities::ACCELEROMETER, "accelerometer"),
            (polymer_input::Capabilities::GYROSCOPE, "gyroscope"),
            (polymer_input::Capabilities::ORIENTATION, "orientation"),
            (polymer_input::Capabilities::TOUCHPAD, "touchpad"),
            (polymer_input::Capabilities::BATTERY, "battery"),
            (polymer_input::Capabilities::RAW_HID, "rawHid"),
            (polymer_input::Capabilities::RECENTER, "recenter"),
            (
                polymer_input::Capabilities::POINTER_RELATIVE,
                "pointerRelative",
            ),
            (
                polymer_input::Capabilities::POINTER_ANGULAR,
                "pointerAngular",
            ),
            (
                polymer_input::Capabilities::POINTER_SURFACE,
                "pointerSurface",
            ),
            (
                polymer_input::Capabilities::POINTER_CONTACT,
                "pointerContact",
            ),
            (
                polymer_input::Capabilities::CALIBRATE_STICK_CENTER,
                "calibrateStickCenter",
            ),
            (
                polymer_input::Capabilities::CALIBRATE_STICK_RANGE,
                "calibrateStickRange",
            ),
            (
                polymer_input::Capabilities::CALIBRATE_DEAD_ZONE,
                "calibrateDeadZone",
            ),
            (
                polymer_input::Capabilities::CALIBRATE_TRIGGER,
                "calibrateTrigger",
            ),
            (
                polymer_input::Capabilities::CALIBRATE_MOTION_BIAS,
                "calibrateMotionBias",
            ),
            (
                polymer_input::Capabilities::CALIBRATE_MOTION_SCALE,
                "calibrateMotionScale",
            ),
            (
                polymer_input::Capabilities::CALIBRATE_POINTER,
                "calibratePointer",
            ),
        ]
        .into_iter()
        .filter(|(capability, _)| capabilities.contains(*capability))
        .map(|(_, name)| name.to_owned())
        .collect()
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
    pub device: Option<ControllerDevice>,
    #[serde(default)]
    pub mouse: bool,
    #[serde(default)]
    pub projection_mode: ProjectionMode,
    #[serde(default)]
    pub mapping: ControllerBindings,
    #[serde(default)]
    pub pointer: PointerBinding,
}

/// Complete emulator controller topology. Disconnected ports remain present so
/// layouts can change without reallocating virtual gamepads.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerTopology {
    pub mappings: Vec<ControllerMapping>,
}

impl ControllerTopology {
    /// Compact, stable assignment data for cross-layer diagnostics. Input
    /// bindings are intentionally omitted because they make topology-change
    /// logs too large to compare by eye.
    pub fn assignment_diagnostics(&self) -> serde_json::Value {
        self.mappings
            .iter()
            .map(|mapping| {
                serde_json::json!({
                    "target": {
                        "group": mapping.target.group,
                        "port": mapping.target.port,
                    },
                    "gamepad": mapping.gamepad,
                    "connected": mapping.connected,
                    "source": mapping.source,
                    "layout": mapping.layout,
                    "device": mapping.device.as_ref().map(|device| serde_json::json!({
                        "id": device.id,
                        "name": device.name,
                        "profile": device.profile,
                        "vendorId": device.vendor_id,
                        "productId": device.product_id,
                        "serial": device.serial,
                        "role": device.role,
                        "capabilities": device.capabilities,
                    })),
                })
            })
            .collect()
    }

    /// Completes each selected port group with disconnected mappings for its
    /// remaining ports. Alternative groups do not consume virtual gamepads.
    pub fn complete(
        ports: Vec<ControllerPort>,
        selected: Vec<ControllerMapping>,
    ) -> Result<Self, String> {
        let selected_groups = selected
            .iter()
            .map(|mapping| mapping.target.group.as_str())
            .collect::<HashSet<_>>();
        let ports = ports
            .into_iter()
            .filter(|port| selected_groups.contains(port.group.as_str()))
            .collect::<Vec<_>>();
        if ports.len() > 8 {
            return Err("an emulator cannot expose more than 8 virtual gamepads".to_owned());
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
                    pointer: PointerBinding {
                        mode: port.pointer,
                        ..PointerBinding::default()
                    },
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
            if mapping.gamepad >= 8 {
                return Err(format!(
                    "virtual gamepad {} exceeds the maximum player index",
                    mapping.gamepad
                ));
            }
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
            layout.validate_controller_bindings(&mapping.mapping)?;
            let mut binding_targets = HashSet::new();
            for (target, _) in mapping.mapping.iter() {
                binding_targets.insert(*target);
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
            if mapping.pointer.mode == PointerMode::AbsoluteDevice {
                match mapping.pointer.source {
                    PointerSource::HostMouse => {
                        return Err(format!(
                            "host mouse cannot provide absolute-device input for {}:{}",
                            mapping.target.group, mapping.target.port
                        ));
                    }
                    PointerSource::DeviceNative | PointerSource::DeviceThenHostMouse
                        if mapping.device.as_ref().is_some_and(|device| {
                            device
                                .capabilities
                                .iter()
                                .any(|capability| capability == "pointerRelative")
                                && !device.capabilities.iter().any(|capability| {
                                    matches!(
                                        capability.as_str(),
                                        "pointerAngular" | "pointerSurface"
                                    )
                                })
                        }) =>
                    {
                        return Err(format!(
                            "relative-only device cannot provide absolute-device input for {}:{}",
                            mapping.target.group, mapping.target.port
                        ));
                    }
                    _ => {}
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
        let hub = ControllerConfiguration::new(None, []).expect("an empty topology is valid");
        hub.preview(&self.input_transaction(machine, 0)?)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub fn input_topology(
        &self,
        revision: u32,
        machine: Option<String>,
    ) -> Result<InputTopology, String> {
        self.validate()?;
        let ports = self
            .mappings
            .iter()
            .map(|mapping| {
                let mode = match mapping.projection_mode {
                    ProjectionMode::Position => RemapMode::Position,
                    ProjectionMode::Name => RemapMode::Name,
                    ProjectionMode::Custom => RemapMode::Custom,
                };
                let port = u8::try_from(mapping.target.port).map_err(|_| {
                    format!(
                        "controller target {}:{} exceeds 255 ports",
                        mapping.target.group, mapping.target.port
                    )
                })?;
                Ok(PortBinding {
                    player: mapping.gamepad,
                    keyboard_mouse: mapping.connected
                        && matches!(mapping.source, Some(SourceSelector::KeyboardMouse)),
                    group: mapping.target.group.clone(),
                    port,
                    layout: mapping.layout.clone(),
                    mode,
                    bindings: if mode == RemapMode::Custom {
                        mapping
                            .mapping
                            .iter()
                            .map(|(target, source)| {
                                (target.to_string(), source.map(ToString::to_string))
                            })
                            .collect()
                    } else {
                        Vec::new()
                    },
                    pointer: mapping.pointer,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(InputTopology {
            revision,
            machine,
            ports,
            raw_hid: Vec::new(),
        })
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
                overrides: mapping.mapping.clone(),
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

    pub fn port_assignments(&self) -> Result<polymer_config::ast::PortAssignments, String> {
        self.validate()?;
        let mut assignments =
            BTreeMap::<String, BTreeMap<usize, polymer_config::ast::PortAssignment>>::new();
        for mapping in &self.mappings {
            assignments
                .entry(mapping.target.group.clone())
                .or_default()
                .insert(
                    mapping.target.port,
                    polymer_config::ast::PortAssignment {
                        layout: mapping.layout.clone().into_boxed_str(),
                        mouse: matches!(mapping.source, Some(SourceSelector::KeyboardMouse)),
                    },
                );
        }
        assignments
            .into_iter()
            .map(|(group, layouts)| {
                let layouts = layouts
                    .into_iter()
                    .enumerate()
                    .map(|(expected, (port, assignment))| {
                        (port == expected).then_some(assignment).ok_or_else(|| {
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
