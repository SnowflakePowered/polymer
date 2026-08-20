//! Native configuration and launch-selection services.

use serde::{Deserialize, Serialize};
use snowglass_config::ast::{
    ConfigurationInstance, OptionRange, PortAssignments, Value, ValueKind,
};
use snowglass_input::{hub::VirtualPort, provider::sdl3::DeviceSelector};
use snowglass_pluginhost::{EmulatorPluginInstance, EmulatorPort, PackageError};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
};

#[derive(Clone, Debug)]
pub struct ConfigurationDefaults {
    pub gamepads: Vec<DefaultGamepad>,
    pub machine: String,
    pub webrtc_targets: Option<Vec<u32>>,
    pub webrtc_bind: std::net::SocketAddr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefaultGamepad {
    pub selector: DeviceSelector,
    pub slot: usize,
    pub layout: String,
    pub force_mouse: bool,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GamepadMapping {
    pub gamepad_id: GamepadId,
    #[serde(default)]
    pub mouse: bool,
    pub slot: usize,
    #[serde(default)]
    pub port: Option<VirtualPort>,
    pub layout: String,
    #[serde(default)]
    pub mapping: HashMap<String, String>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum GamepadId {
    Index(usize),
    Name(String),
}

impl GamepadMapping {
    pub fn selector(&self) -> DeviceSelector {
        match &self.gamepad_id {
            GamepadId::Index(index) => DeviceSelector::Index(*index),
            GamepadId::Name(value) if value == "K" => DeviceSelector::Keyboard,
            GamepadId::Name(value) => {
                let compact = value.replace('-', "");
                if compact.len() == 32 && compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    DeviceSelector::Guid(compact.to_ascii_lowercase())
                } else {
                    DeviceSelector::Name(value.clone())
                }
            }
        }
    }
}

impl From<&DefaultGamepad> for GamepadMapping {
    fn from(value: &DefaultGamepad) -> Self {
        let gamepad_id = match &value.selector {
            DeviceSelector::Keyboard => GamepadId::Name("K".to_owned()),
            DeviceSelector::Index(index) => GamepadId::Index(*index),
            DeviceSelector::Name(name) | DeviceSelector::Guid(name) => {
                GamepadId::Name(name.clone())
            }
            DeviceSelector::GuidOrdinal { guid, .. } => GamepadId::Name(guid.clone()),
            DeviceSelector::DeviceId(device_id) => GamepadId::Name(device_id.clone()),
        };
        Self {
            gamepad_id,
            mouse: value.force_mouse,
            slot: value.slot,
            port: None,
            layout: value.layout.clone(),
            mapping: HashMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedGame {
    pub path: PathBuf,
    pub platform: String,
    pub preferred_emulator: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameDescriptor {
    pub path: String,
    pub name: String,
    pub platform: String,
    pub selected_emulator: String,
    pub candidates: Vec<EmulatorDescriptor>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmulatorDescriptor {
    pub id: String,
    pub groups: Vec<PortGroupDescriptor>,
    pub options: Vec<OptionDescriptor>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortGroupDescriptor {
    pub name: String,
    pub connected: usize,
    pub ports: Vec<PortDescriptor>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortDescriptor {
    pub index: usize,
    pub layouts: Vec<String>,
    pub selected_layout: String,
    pub selected_device: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionDescriptor {
    pub path: String,
    pub title: String,
    pub kind: String,
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<ChoiceDescriptor>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ChoiceDescriptor {
    pub key: String,
    pub label: String,
}

pub struct PreparedGame {
    descriptor: GameDescriptor,
    viewer: ConfigurationDefaults,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigurationRequest {
    pub emulator: String,
    pub groups: Vec<LaunchGroupRequest>,
    #[serde(default)]
    pub settings: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub nand: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LaunchGroupRequest {
    pub name: String,
    pub ports: Vec<LaunchPortRequest>,
}

#[derive(Debug, Deserialize)]
pub struct LaunchPortRequest {
    pub index: usize,
    pub layout: String,
    pub device: String,
}

pub struct ConfiguredSelection {
    pub viewer: ConfigurationDefaults,
    pub port_assignments: PortAssignments,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreparedConfiguration {
    plugin: String,
    arguments: serde_json::Value,
    gamepad_mapping: Vec<GamepadMapping>,
    machine: String,
    webrtc_bind: Option<String>,
    webrtc_targets: Vec<u32>,
}

pub fn describe_api(
    plugins: snowglass_pluginhost::PluginRegistry,
    viewer: ConfigurationDefaults,
    package: serde_json::Value,
    preferred_emulator: Option<String>,
) -> Result<serde_json::Value, String> {
    let package: crate::ScrapedPackage =
        serde_json::from_value(package).map_err(|error| error.to_string())?;
    let descriptor = PreparedGame::new(
        DetectedGame {
            path: package.path,
            platform: package.platform,
            preferred_emulator,
        },
        plugins.plugins::<dyn EmulatorPluginInstance>(),
        viewer,
    )?
    .descriptor()
    .clone();
    serde_json::to_value(descriptor).map_err(|error| error.to_string())
}

pub fn prepare_api(
    plugins: snowglass_pluginhost::PluginRegistry,
    viewer: ConfigurationDefaults,
    package: serde_json::Value,
    request: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let package: crate::ScrapedPackage =
        serde_json::from_value(package).map_err(|error| error.to_string())?;
    let request: ConfigurationRequest =
        serde_json::from_value(request).map_err(|error| error.to_string())?;
    let settings = request.settings.clone();
    let emulator = request.emulator.clone();
    let nand = request.nand.clone();
    let prepared = PreparedGame::new(
        DetectedGame {
            path: package.path.clone(),
            platform: package.platform.clone(),
            preferred_emulator: Some(emulator.clone()),
        },
        plugins.plugins::<dyn EmulatorPluginInstance>(),
        viewer,
    )?
    .prepare(request)?;
    let port_assignments = prepared
        .port_assignments
        .iter()
        .map(|(group, layouts)| {
            (
                group.to_string(),
                layouts.iter().map(ToString::to_string).collect::<Vec<_>>(),
            )
        })
        .collect::<HashMap<_, _>>();
    let webrtc_targets = prepared.viewer.webrtc_targets.clone().unwrap_or_default();
    let mut arguments = serde_json::json!({
        "launchPath": package.path.to_string_lossy(),
        "package": package,
        "console": prepared.viewer.machine,
        "portAssignments": port_assignments,
        "settings": settings,
    });
    if let Some(id) = nand {
        arguments["nand"] = serde_json::json!({
            "platform": prepared.viewer.machine,
            "id": id,
        });
    }
    serde_json::to_value(PreparedConfiguration {
        plugin: emulator,
        arguments,
        gamepad_mapping: prepared
            .viewer
            .gamepads
            .iter()
            .map(GamepadMapping::from)
            .collect(),
        machine: prepared.viewer.machine,
        webrtc_bind: prepared
            .viewer
            .webrtc_targets
            .as_ref()
            .map(|_| prepared.viewer.webrtc_bind.to_string()),
        webrtc_targets,
    })
    .map_err(|error| error.to_string())
}

impl PreparedGame {
    pub fn new(
        detected: DetectedGame,
        plugins: impl IntoIterator<Item = Arc<dyn EmulatorPluginInstance>>,
        viewer: ConfigurationDefaults,
    ) -> Result<Self, String> {
        let mut plugins = plugins
            .into_iter()
            .filter(|plugin| plugin.platforms().contains(&detected.platform))
            .collect::<Vec<_>>();
        if let Some(preferred) = &detected.preferred_emulator {
            plugins.retain(|plugin| plugin.id() == preferred);
            if plugins.is_empty() {
                return Err(format!(
                    "Emulator plugin '{preferred}' does not support detected platform {}.",
                    detected.platform
                ));
            }
        }
        if plugins.is_empty() {
            return Err(format!(
                "No loaded emulator plugin supports {}.",
                detected.platform
            ));
        }

        let mut descriptors = Vec::new();
        let mut errors = Vec::new();
        for plugin in plugins {
            match describe_emulator(plugin.as_ref(), &detected.platform, &viewer) {
                Ok(descriptor) => {
                    descriptors.push(descriptor);
                }
                Err(error) => errors.push(format!("{}: {error}", plugin.id())),
            }
        }
        if descriptors.is_empty() {
            return Err(format!(
                "No compatible emulator could be configured for {}: {}",
                detected.platform,
                errors.join("; ")
            ));
        }

        let name = display_name(&detected.path);
        let selected_emulator = descriptors[0].id.clone();
        Ok(Self {
            descriptor: GameDescriptor {
                path: detected.path.to_string_lossy().into_owned(),
                name,
                platform: detected.platform,
                selected_emulator,
                candidates: descriptors,
            },
            viewer,
        })
    }

    pub fn descriptor(&self) -> &GameDescriptor {
        &self.descriptor
    }

    pub fn prepare(&self, request: ConfigurationRequest) -> Result<ConfiguredSelection, String> {
        let candidate_index = self
            .descriptor
            .candidates
            .iter()
            .position(|candidate| candidate.id == request.emulator)
            .ok_or_else(|| {
                format!(
                    "Emulator '{}' is not available for this game.",
                    request.emulator
                )
            })?;
        let descriptor = &self.descriptor.candidates[candidate_index];
        let (assignments, gamepads) = validate_assignments(descriptor, request.groups)?;
        let mut viewer = self.viewer.clone();
        viewer.machine.clone_from(&self.descriptor.platform);
        viewer.gamepads = gamepads;
        Ok(ConfiguredSelection {
            viewer,
            port_assignments: assignments,
        })
    }
}

fn describe_emulator(
    plugin: &dyn EmulatorPluginInstance,
    platform: &str,
    viewer: &ConfigurationDefaults,
) -> Result<EmulatorDescriptor, String> {
    let ports = match plugin.ports(platform) {
        Ok(ports) => ports,
        Err(PackageError::NoInputDefinition) => Vec::new(),
        Err(error) => return Err(error.to_string()),
    };
    let groups = default_groups(ports, viewer, plugin, platform);
    let assignments = descriptor_assignments(&groups);
    let configuration = plugin
        .materialize(platform, assignments)
        .map_err(|error| error.to_string())?;
    Ok(EmulatorDescriptor {
        id: plugin.id().to_owned(),
        groups,
        options: describe_options(&configuration),
    })
}

fn default_groups(
    ports: Vec<EmulatorPort>,
    viewer: &ConfigurationDefaults,
    plugin: &dyn EmulatorPluginInstance,
    platform: &str,
) -> Vec<PortGroupDescriptor> {
    let requested_layouts = viewer
        .gamepads
        .iter()
        .map(|gamepad| gamepad.layout.clone())
        .collect::<Vec<_>>();
    let requested = plugin
        .port_assignments(platform, &requested_layouts)
        .unwrap_or_default();
    let mut grouped = BTreeMap::<String, Vec<EmulatorPort>>::new();
    for port in ports {
        grouped.entry(port.group.clone()).or_default().push(port);
    }
    let mut source_slot = 0;
    let mut groups = grouped
        .into_iter()
        .map(|(name, mut ports)| {
            ports.sort_by_key(|port| port.index);
            let assigned = requested.get(name.as_str());
            let connected = assigned.map_or(0, |layouts| layouts.len()).min(ports.len());
            let ports = ports
                .into_iter()
                .map(|port| {
                    let selected_layout = assigned
                        .and_then(|layouts| layouts.get(port.index))
                        .and_then(|layout| {
                            port.layouts
                                .iter()
                                .find(|candidate| candidate.as_str() == layout.as_ref())
                        })
                        .cloned()
                        .unwrap_or_else(|| port.layouts[0].clone());
                    let selected_device = default_device_for_port(
                        &viewer.gamepads,
                        port.index < connected,
                        port.index,
                        &mut source_slot,
                    );
                    PortDescriptor {
                        index: port.index,
                        layouts: port.layouts,
                        selected_layout,
                        selected_device,
                    }
                })
                .collect();
            PortGroupDescriptor {
                name,
                connected,
                ports,
            }
        })
        .collect::<Vec<_>>();
    if groups.iter().all(|group| group.connected == 0)
        && let Some(group) = groups.first_mut()
    {
        group.connected = usize::from(!group.ports.is_empty());
    }
    groups
}

fn default_device_for_port(
    gamepads: &[DefaultGamepad],
    connected: bool,
    port_index: usize,
    source_slot: &mut usize,
) -> String {
    if !connected {
        // Merely describing an available port must not consume a requested
        // input source. Machines such as the Wii U list their optional
        // controller group before the connected GamePad group.
        return port_index.to_string();
    }
    let selected = gamepads
        .get(*source_slot)
        .map_or_else(|| source_slot.to_string(), selector_text);
    *source_slot += 1;
    selected
}

fn descriptor_assignments(groups: &[PortGroupDescriptor]) -> PortAssignments {
    groups
        .iter()
        .filter(|group| group.connected != 0)
        .map(|group| {
            (
                group.name.clone().into_boxed_str(),
                group.ports[..group.connected]
                    .iter()
                    .map(|port| port.selected_layout.clone().into_boxed_str())
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            )
        })
        .collect()
}

fn describe_options(configuration: &ConfigurationInstance) -> Vec<OptionDescriptor> {
    configuration
        .into_iter()
        .map(|setting| {
            let option = configuration.definition().option(setting.option);
            let (kind, min, max, choices) = match &option.definition.value.kind {
                ValueKind::Boolean => ("boolean", None, None, Vec::new()),
                ValueKind::Integer { range } => {
                    let range: Option<(serde_json::Value, serde_json::Value)> =
                        range.map(|range| {
                            let OptionRange::Integer { min, max } = range else {
                                unreachable!()
                            };
                            (min.into(), max.into())
                        });
                    (
                        "integer",
                        range.as_ref().map(|value| value.0.clone()),
                        range.map(|value| value.1),
                        Vec::new(),
                    )
                }
                ValueKind::Float { range } => {
                    let range: Option<(serde_json::Value, serde_json::Value)> =
                        range.map(|range| {
                            let OptionRange::Float { min, max } = range else {
                                unreachable!()
                            };
                            (serde_json::json!(min), serde_json::json!(max))
                        });
                    (
                        "float",
                        range.as_ref().map(|value| value.0.clone()),
                        range.map(|value| value.1),
                        Vec::new(),
                    )
                }
                ValueKind::String => ("string", None, None, Vec::new()),
                ValueKind::Path => ("path", None, None, Vec::new()),
                ValueKind::Select { .. } => (
                    "select",
                    None,
                    None,
                    configuration
                        .definition()
                        .choices(setting.option)
                        .unwrap_or_default()
                        .iter()
                        .map(|choice| ChoiceDescriptor {
                            key: choice.key.as_str().to_owned(),
                            label: choice.label.to_string(),
                        })
                        .collect(),
                ),
            };
            OptionDescriptor {
                path: setting.path.as_str().to_owned(),
                title: option.definition.title.to_string(),
                kind: kind.to_owned(),
                value: value_to_json(&setting.value),
                min,
                max,
                choices,
            }
        })
        .collect()
}

fn validate_assignments(
    descriptor: &EmulatorDescriptor,
    groups: Vec<LaunchGroupRequest>,
) -> Result<(PortAssignments, Vec<DefaultGamepad>), String> {
    let mut by_name = groups
        .into_iter()
        .map(|group| (group.name.clone(), group))
        .collect::<BTreeMap<_, _>>();
    let mut assignments = PortAssignments::new();
    let mut gamepads = Vec::new();
    for described_group in &descriptor.groups {
        let Some(group) = by_name.remove(&described_group.name) else {
            continue;
        };
        if group.ports.len() > described_group.ports.len() {
            return Err(format!(
                "{} accepts at most {} controllers.",
                group.name,
                described_group.ports.len()
            ));
        }
        let mut layouts = Vec::new();
        for (expected_index, port) in group.ports.into_iter().enumerate() {
            if port.index != expected_index {
                return Err(format!(
                    "{} controller ports must be contiguous from zero.",
                    group.name
                ));
            }
            let described_port = &described_group.ports[expected_index];
            if !described_port.layouts.contains(&port.layout) {
                return Err(format!(
                    "{} {} does not accept layout '{}'.",
                    group.name,
                    expected_index + 1,
                    port.layout
                ));
            }
            let selector = parse_device(&port.device)?;
            layouts.push(port.layout.clone().into_boxed_str());
            gamepads.push(DefaultGamepad {
                force_mouse: matches!(selector, DeviceSelector::Keyboard),
                selector,
                slot: gamepads.len(),
                layout: port.layout,
            });
        }
        if !layouts.is_empty() {
            assignments.insert(group.name.into_boxed_str(), layouts.into_boxed_slice());
        }
    }
    if let Some((name, _)) = by_name.into_iter().next() {
        return Err(format!("Emulator has no input group '{name}'."));
    }
    Ok((assignments, gamepads))
}

fn selector_text(gamepad: &DefaultGamepad) -> String {
    match &gamepad.selector {
        DeviceSelector::Keyboard => "K".to_owned(),
        DeviceSelector::Index(index) => index.to_string(),
        DeviceSelector::Name(name) => name.clone(),
        DeviceSelector::Guid(guid) => guid.clone(),
        DeviceSelector::GuidOrdinal {
            guid,
            duplicate_ordinal,
        } => format!("{guid}#{duplicate_ordinal}"),
        DeviceSelector::DeviceId(id) => id.clone(),
    }
}

fn parse_device(device: &str) -> Result<DeviceSelector, String> {
    if device == "K" {
        Ok(DeviceSelector::Keyboard)
    } else if let Ok(index) = device.parse::<usize>() {
        Ok(DeviceSelector::Index(index))
    } else if device.trim().is_empty() {
        Err("Controller source cannot be empty.".to_owned())
    } else {
        Ok(DeviceSelector::Name(device.to_owned()))
    }
}

fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Boolean(value) => (*value).into(),
        Value::Integer(value) => (*value).into(),
        Value::Float(value) => serde_json::json!(value),
        Value::String(value) | Value::Choice(value) => value.as_ref().into(),
        Value::Path(value) => value.to_string_lossy().into_owned().into(),
    }
}

fn display_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnected_ports_do_not_consume_requested_input_sources() {
        let gamepads = [DefaultGamepad {
            selector: DeviceSelector::Keyboard,
            slot: 0,
            layout: "WIIU_DRC_CONTROLLER".to_owned(),
            force_mouse: true,
        }];
        let mut source_slot = 0;

        for port_index in 0..7 {
            assert_eq!(
                default_device_for_port(&gamepads, false, port_index, &mut source_slot),
                port_index.to_string()
            );
        }
        assert_eq!(source_slot, 0);
        assert_eq!(
            default_device_for_port(&gamepads, true, 0, &mut source_slot),
            "K"
        );
        assert_eq!(source_slot, 1);
    }
}
