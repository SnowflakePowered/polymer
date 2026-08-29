//! Native configuration and launch-selection services.

use serde::{Deserialize, Serialize};
use snowglass_config::ast::{
    ConfigurationInstance, OptionRange, PortAssignments, Value, ValueKind,
};
use snowglass_input::{
    hub::{SourceSelector, VirtualPort},
    layout::ProjectionMode,
    source::InputSourceCatalog,
    topology::{ControllerBindings, ControllerMapping, ControllerTarget, ControllerTopology},
};
use snowglass_pluginhost::{EmulatorPluginInstance, EmulatorPort, PackageError};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
};

#[derive(Clone, Debug)]
pub struct ConfigurationDefaults {
    pub gamepads: Vec<DefaultGamepad>,
    pub webrtc_targets: Option<Vec<u32>>,
    pub webrtc_bind: std::net::SocketAddr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefaultGamepad {
    pub source: Option<SourceSelector>,
    pub slot: usize,
    pub port: Option<VirtualPort>,
    pub layout: String,
    pub force_mouse: bool,
    pub mapping: HashMap<String, String>,
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
    pub storage: BTreeMap<String, serde_json::Value>,
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
    #[serde(default)]
    pub mapping: HashMap<String, String>,
}

pub struct ConfiguredSelection {
    pub viewer: ConfigurationDefaults,
    pub machine: String,
    pub port_assignments: PortAssignments,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreparedConfiguration {
    plugin: String,
    arguments: serde_json::Value,
    controller_topology: ControllerTopology,
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
    let controller_profiles = crate::ControllerProfiles::default();
    let controller_assignments = crate::controllers::ControllerAssignmentSnapshot::default();
    prepare_api_with_sources(
        plugins,
        viewer,
        package,
        request,
        &InputSourceCatalog::default(),
        &controller_profiles,
        &controller_assignments,
    )
}

pub(crate) fn prepare_api_with_sources(
    plugins: snowglass_pluginhost::PluginRegistry,
    viewer: ConfigurationDefaults,
    package: serde_json::Value,
    request: serde_json::Value,
    sources: &InputSourceCatalog,
    controller_profiles: &crate::ControllerProfiles,
    controller_assignments: &crate::controllers::ControllerAssignmentSnapshot,
) -> Result<serde_json::Value, String> {
    let package: crate::ScrapedPackage =
        serde_json::from_value(package).map_err(|error| error.to_string())?;
    let request: ConfigurationRequest =
        serde_json::from_value(request).map_err(|error| error.to_string())?;
    let settings = request.settings.clone();
    let emulator = request.emulator.clone();
    let storage = request.storage.clone();
    let mut prepared = PreparedGame::new(
        DetectedGame {
            path: package.path.clone(),
            platform: package.platform.clone(),
            preferred_emulator: Some(emulator.clone()),
        },
        plugins.plugins::<dyn EmulatorPluginInstance>(),
        viewer,
    )?
    .prepare(request)?;
    for gamepad in &mut prepared.viewer.gamepads {
        gamepad.source = controller_assignments.source_for(gamepad.slot);
        gamepad.force_mouse = matches!(gamepad.source, Some(SourceSelector::KeyboardMouse));
    }
    let webrtc_targets = prepared.viewer.webrtc_targets.clone().unwrap_or_default();
    let controller_manager = crate::ControllerManager::with_profiles(controller_profiles.clone());
    let selected = prepared
        .viewer
        .gamepads
        .iter()
        .map(|gamepad| resolved_controller(gamepad, sources, &controller_manager))
        .collect::<Result<Vec<_>, _>>()?;
    let plugin = plugins
        .plugin::<dyn EmulatorPluginInstance>(&emulator)
        .ok_or_else(|| format!("unknown emulator plugin '{emulator}'"))?;
    let ports = plugin
        .ports(&prepared.machine)
        .map_err(|error| error.to_string())?;
    let topology = ControllerTopologyBuilder::new(ports, selected).build()?;
    let mut arguments = serde_json::json!({
        "launchPath": package.path.to_string_lossy(),
        "package": package,
        "console": prepared.machine,
        "controllerTopology": &topology,
        "settings": settings,
    });
    if !storage.is_empty() {
        arguments["storage"] =
            serde_json::to_value(storage).expect("string storage selections are JSON-compatible");
    }
    serde_json::to_value(PreparedConfiguration {
        plugin: emulator,
        arguments,
        controller_topology: topology,
        machine: prepared.machine,
        webrtc_bind: prepared
            .viewer
            .webrtc_targets
            .as_ref()
            .map(|_| prepared.viewer.webrtc_bind.to_string()),
        webrtc_targets,
    })
    .map_err(|error| error.to_string())
}

/// The controller view passed to plugins is resolved once, before configuration
/// materialization. Plugins can therefore make mapping-dependent configuration
/// choices without rendering or inspecting emulator files.
fn resolved_controller(
    gamepad: &DefaultGamepad,
    sources: &InputSourceCatalog,
    controllers: &crate::ControllerManager,
) -> Result<ControllerMapping, String> {
    let port = gamepad
        .port
        .as_ref()
        .ok_or_else(|| format!("configured gamepad {} has no emulator port", gamepad.slot))?;
    let gamepad_index = u8::try_from(gamepad.slot)
        .map_err(|_| format!("virtual gamepad {} is outside the wire range", gamepad.slot))?;
    let device = match &gamepad.source {
        Some(SourceSelector::Device { device_id }) => sources.get(device_id),
        Some(SourceSelector::KeyboardMouse) | None => None,
    };
    let (projection_mode, mut mapping) = if let Some(device) = &device {
        let profile = controllers.mapping_for_device(device, &gamepad.layout);
        (profile.mode, profile.mapping.into_iter().collect())
    } else {
        (ProjectionMode::Position, HashMap::new())
    };

    if device.is_none()
        && let Some(layout) = snowglass_input::layout::InputLayout::get(&gamepad.layout)
    {
        for element in &layout.elements {
            if let Some(source) = layout.sdl_key(element.key) {
                mapping.insert(element.key.to_string(), source.to_string());
            }
        }
    }

    // Explicit launch mappings always win, including `disabled`.
    if device.is_none() {
        mapping.extend(gamepad.mapping.iter().map(|(target, source)| {
            (
                target.clone(),
                if source == "disabled" {
                    "none".to_owned()
                } else {
                    source.clone()
                },
            )
        }));
    }
    Ok(ControllerMapping {
        target: ControllerTarget {
            group: port.group.clone(),
            port: usize::from(port.index),
        },
        gamepad: gamepad_index,
        source: gamepad.source.clone(),
        connected: device.as_ref().is_some_and(|device| device.connected)
            || matches!(gamepad.source, Some(SourceSelector::KeyboardMouse)),
        mouse: gamepad.force_mouse,
        layout: gamepad.layout.clone(),
        projection_mode,
        mapping: ControllerBindings::try_from_strings(mapping)?,
    })
}

struct ControllerTopologyBuilder {
    ports: Vec<EmulatorPort>,
    selected: Vec<ControllerMapping>,
}

impl ControllerTopologyBuilder {
    fn new(ports: Vec<EmulatorPort>, selected: Vec<ControllerMapping>) -> Self {
        Self { ports, selected }
    }

    fn build(self) -> Result<ControllerTopology, String> {
        if self.ports.len() > u8::MAX as usize + 1 {
            return Err("an emulator cannot expose more than 256 virtual gamepads".to_owned());
        }
        let mut used = vec![false; self.ports.len()];
        for mapping in &self.selected {
            let port = self.port_for(&mapping.target)?;
            if !port.layouts.contains(&mapping.layout) {
                return Err(format!(
                    "layout '{}' is not supported by {}:{}",
                    mapping.layout, mapping.target.group, mapping.target.port
                ));
            }
            let slot = usize::from(mapping.gamepad);
            if slot >= used.len() || std::mem::replace(&mut used[slot], true) {
                return Err(format!(
                    "virtual gamepad {} is invalid or duplicated",
                    mapping.gamepad
                ));
            }
        }
        let mut available = used
            .iter()
            .enumerate()
            .filter_map(|(slot, used)| (!used).then_some(slot));
        let mappings = self
            .ports
            .iter()
            .map(|port| {
                self.selected
                    .iter()
                    .find(|mapping| {
                        mapping.target.group == port.group && mapping.target.port == port.index
                    })
                    .cloned()
                    .map(Ok)
                    .unwrap_or_else(|| {
                        let slot = available.next().expect("unused ports have unused gamepads");
                        Ok(ControllerMapping {
                            target: ControllerTarget {
                                group: port.group.clone(),
                                port: port.index,
                            },
                            gamepad: slot as u8,
                            source: None,
                            layout: port.layouts.first().cloned().ok_or_else(|| {
                                format!(
                                    "emulator port {}:{} declares no layouts",
                                    port.group, port.index
                                )
                            })?,
                            connected: false,
                            mouse: false,
                            projection_mode: ProjectionMode::Position,
                            mapping: ControllerBindings::default(),
                        })
                    })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(ControllerTopology { mappings })
    }

    fn port_for(&self, target: &ControllerTarget) -> Result<&EmulatorPort, String> {
        self.ports
            .iter()
            .find(|port| port.group == target.group && port.index == target.port)
            .ok_or_else(|| format!("unknown emulator port {}:{}", target.group, target.port))
    }
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
        viewer.gamepads = gamepads;
        Ok(ConfiguredSelection {
            viewer,
            machine: self.descriptor.platform.clone(),
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
    let mut ordered = Vec::new();
    if let Ok(machine) = consolespec::MachineSpec::try_from(platform) {
        for group in machine.input_groups() {
            if let Some(ports) = grouped.remove(group.name()) {
                ordered.push((group.name().to_owned(), ports));
            }
        }
    }
    ordered.extend(grouped);
    let mut groups = ordered
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
                    PortDescriptor {
                        index: port.index,
                        layouts: port.layouts,
                        selected_layout,
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
            layouts.push(port.layout.clone().into_boxed_str());
            gamepads.push(DefaultGamepad {
                force_mouse: false,
                source: None,
                slot: gamepads.len(),
                port: Some(VirtualPort::new(
                    group.name.clone(),
                    u8::try_from(port.index)
                        .map_err(|_| format!("{} controller index is too large.", group.name))?,
                )),
                layout: port.layout,
                mapping: port.mapping,
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
