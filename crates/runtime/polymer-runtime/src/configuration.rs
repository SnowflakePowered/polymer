//! Native configuration and launch-selection services.

use crate::{ControllerAssignmentSnapshot, ControllerManager, ControllerProfiles};
use polymer_config::ast::{PortAssignment, PortAssignments};
use polymer_config::input::{ControllerBindings, ProjectionMode, SourceSelector, VirtualPort};
use polymer_controllers::{ControllerMapping, ControllerTarget, ControllerTopology};
use polymer_input::InputHub;
use polymer_pluginhost::{
    emulator::{EmulatorPluginInstance, EmulatorPort},
    loading::{PackageError, PluginRegistry},
    runtime::PluginRuntimeHandle,
};
use polymer_scraping::Package;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

#[derive(Clone, Debug)]
pub struct ConfigurationDefaults {
    pub gamepads: Vec<DefaultGamepad>,
    pub webrtc_targets: Option<Vec<u32>>,
    pub webrtc_bind: std::net::SocketAddr,
}

impl Default for ConfigurationDefaults {
    fn default() -> Self {
        Self {
            gamepads: Vec::new(),
            webrtc_targets: None,
            webrtc_bind: std::net::SocketAddr::from(([127, 0, 0, 1], 8080)),
        }
    }
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
    pub name: String,
    pub uri: polymer_library::LaunchableUri,
    pub platform: String,
    pub preferred_emulator: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameDescriptor {
    pub path: polymer_library::LaunchableUri,
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
    pub configuration: Option<serde_json::Value>,
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
    pub pointer: polymer_controllers::topology::PointerMode,
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
    pub user_configuration: BTreeMap<String, BTreeMap<String, serde_json::Value>>,
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

pub async fn describe_api(
    plugins: PluginRuntimeHandle,
    viewer: ConfigurationDefaults,
    package: Package,
    preferred_emulator: Option<String>,
) -> Result<serde_json::Value, String> {
    let mut prepared = PreparedGame::new(
        DetectedGame {
            name: package.filename.clone(),
            uri: package.uri.clone(),
            platform: package.platform.clone(),
            preferred_emulator: preferred_emulator.clone(),
        },
        plugins.registry().plugins::<dyn EmulatorPluginInstance>(),
        viewer,
    )?;
    let game = serde_json::to_value(&package).map_err(|error| error.to_string())?;
    let mut errors = Vec::new();
    let mut retained = Vec::new();
    for mut candidate in prepared.descriptor.candidates.drain(..) {
        match plugins.configure(&candidate.id, game.clone(), None).await {
            Ok(configuration) => {
                candidate.configuration = configuration;
                retained.push(candidate);
            }
            Err(error) if preferred_emulator.as_deref() == Some(candidate.id.as_str()) => {
                return Err(format!("{}: {error}", candidate.id));
            }
            Err(error) => errors.push(format!("{}: {error}", candidate.id)),
        }
    }
    if retained.is_empty() {
        return Err(format!(
            "No compatible emulator could be configured for {}: {}",
            package.platform,
            errors.join("; ")
        ));
    }
    prepared.descriptor.selected_emulator = retained[0].id.clone();
    prepared.descriptor.candidates = retained;
    serde_json::to_value(prepared.descriptor).map_err(|error| error.to_string())
}

pub async fn configure_api(
    plugins: PluginRuntimeHandle,
    package: Package,
    emulator: String,
    user_configuration: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let plugin = plugins
        .registry()
        .plugin::<dyn EmulatorPluginInstance>(&emulator)
        .ok_or_else(|| format!("unknown emulator plugin '{emulator}'"))?;
    if !plugin.platforms().contains(&package.platform) {
        return Err(format!(
            "Emulator plugin '{emulator}' does not support detected platform {}.",
            package.platform
        ));
    }
    let layout = plugins
        .configure(
            &emulator,
            serde_json::to_value(&package).map_err(|error| error.to_string())?,
            user_configuration,
        )
        .await
        .map_err(|error| error.to_string())?;
    serde_json::to_value(layout).map_err(|error| error.to_string())
}

pub fn prepare_api(
    plugins: PluginRegistry,
    viewer: ConfigurationDefaults,
    package: Package,
    request: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let controller_profiles = ControllerProfiles::default();
    let controller_assignments = ControllerAssignmentSnapshot::default();
    prepare_api_with_sources(
        plugins,
        viewer,
        package,
        request,
        &InputHub::default(),
        &controller_profiles,
        &controller_assignments,
    )
}

pub(crate) fn prepare_api_with_sources(
    plugins: PluginRegistry,
    viewer: ConfigurationDefaults,
    mut package: Package,
    request: serde_json::Value,
    sources: &InputHub,
    controller_profiles: &ControllerProfiles,
    controller_assignments: &ControllerAssignmentSnapshot,
) -> Result<serde_json::Value, String> {
    package
        .prepare_launch()
        .map_err(|error| error.to_string())?;
    let request: ConfigurationRequest =
        serde_json::from_value(request).map_err(|error| error.to_string())?;
    let user_configuration = request.user_configuration.clone();
    let emulator = request.emulator.clone();
    let storage = request.storage.clone();
    let mut prepared = PreparedGame::new(
        DetectedGame {
            name: package.filename.clone(),
            uri: package.uri.clone(),
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
    let controller_manager = ControllerManager::with_profiles(controller_profiles.clone());
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
    let topology = ControllerTopology::complete(ports, selected)?;
    let entry = package.launch_entry();
    let mut arguments = serde_json::json!({
        "launchSet": [&entry],
        "initialDiscIndex": 0,
        "launchPath": entry["launchPath"],
        "package": entry["package"],
        "console": prepared.machine,
        "controllerTopology": &topology,
        "userConfiguration": user_configuration,
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
    sources: &InputHub,
    controllers: &ControllerManager,
) -> Result<ControllerMapping, String> {
    let port = gamepad
        .port
        .as_ref()
        .ok_or_else(|| format!("configured gamepad {} has no emulator port", gamepad.slot))?;
    let gamepad_index = u8::try_from(gamepad.slot)
        .map_err(|_| format!("virtual gamepad {} is outside the wire range", gamepad.slot))?;
    let device = match &gamepad.source {
        Some(SourceSelector::Device { device_id }) => sources
            .devices()
            .into_iter()
            .find(|(_, device)| device.id.as_str() == device_id)
            .map(|(_, device)| device),
        Some(SourceSelector::KeyboardMouse) | None => None,
    };
    let (projection_mode, mut mapping) = if let Some(device) = &device {
        let profile = controllers.mapping_for_device(device, &gamepad.layout);
        (profile.mode, profile.mapping.into_iter().collect())
    } else {
        (ProjectionMode::Position, HashMap::new())
    };

    if device.is_none()
        && let Some(layout) = polymer_controllers::layout::InputLayout::get(&gamepad.layout)
    {
        for (target, source) in layout.default_gamepad_bindings() {
            mapping.insert(target.to_string(), source.to_string());
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
        connected: device.is_some()
            || matches!(gamepad.source, Some(SourceSelector::KeyboardMouse)),
        device: device
            .as_ref()
            .map(polymer_controllers::ControllerDevice::from_descriptor),
        mouse: gamepad.force_mouse,
        layout: gamepad.layout.clone(),
        projection_mode,
        mapping: ControllerBindings::try_from_strings(mapping)?,
        pointer: polymer_controllers::topology::PointerBinding::default(),
    })
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

        let selected_emulator = descriptors[0].id.clone();
        Ok(Self {
            descriptor: GameDescriptor {
                path: detected.uri,
                name: detected.name,
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
    Ok(EmulatorDescriptor {
        id: plugin.id().to_owned(),
        groups,
        configuration: None,
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
                        .and_then(|assignment| {
                            port.layouts
                                .iter()
                                .find(|candidate| candidate.as_str() == &*assignment.layout)
                        })
                        .cloned()
                        .unwrap_or_else(|| port.layouts[0].clone());
                    PortDescriptor {
                        index: port.index,
                        layouts: port.layouts,
                        selected_layout,
                        pointer: port.pointer,
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
            layouts.push(PortAssignment {
                layout: port.layout.clone().into_boxed_str(),
                mouse: false,
            });
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
