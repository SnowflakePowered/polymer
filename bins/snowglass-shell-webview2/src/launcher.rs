use gumshoe::{GeneralIdentifier, IdentifyPackage, PlatformIdentifier};
use serde::{Deserialize, Serialize};
use snowglass_config::ast::{
    ConfigurationInstance, OptionRange, PortAssignments, Value, ValueKind,
};
use snowglass_input::provider::sdl3::DeviceSelector;
use snowglass_pluginhost::{EmulatorPluginInstance, EmulatorPort, PackageError};
use snowglass_renderer::cli::{GamepadArgument, ViewerOptions};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

#[derive(Debug)]
pub struct DetectedGame {
    pub path: PathBuf,
    pub platform: String,
    pub preferred_emulator: Option<String>,
}

pub async fn detect_game(
    path: PathBuf,
    preferred_emulator: Option<String>,
) -> Result<DetectedGame, String> {
    tokio::task::spawn_blocking(move || {
        let path = path
            .canonicalize()
            .map_err(|error| format!("cannot open game path {}: {error}", path.display()))?;
        let package = GeneralIdentifier
            .identify_path(&path)
            .map_err(|error| format!("could not identify {}: {error}", path.display()))?;
        let platform = consolespec_platform(package.platform()).ok_or_else(|| {
            format!(
                "{} was identified as {:?}, which Snowglass does not map to a consolespec platform",
                path.display(),
                package.platform()
            )
        })?;
        Ok(DetectedGame {
            path,
            platform: platform.to_owned(),
            preferred_emulator,
        })
    })
    .await
    .map_err(|error| format!("game detection task failed: {error}"))?
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LauncherState {
    Idle,
    Detecting { path: String, name: String },
    Configure { game: GameDescriptor },
    Launching { name: String },
    Error { message: String },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameDescriptor {
    pub generation: u64,
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
    candidates: Vec<PreparedEmulator>,
    viewer: ViewerOptions,
    path: PathBuf,
}

struct PreparedEmulator {
    plugin: Arc<dyn EmulatorPluginInstance>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LauncherStartRequest {
    pub generation: u64,
    pub emulator: String,
    pub groups: Vec<LaunchGroupRequest>,
    #[serde(default)]
    pub settings: BTreeMap<String, serde_json::Value>,
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

pub struct ConfiguredGameLaunch {
    pub plugin: Arc<dyn EmulatorPluginInstance>,
    pub game: PathBuf,
    pub viewer: ViewerOptions,
    pub configuration: ConfigurationInstance,
}

impl PreparedGame {
    pub fn new(
        generation: u64,
        detected: DetectedGame,
        plugins: impl IntoIterator<Item = Arc<dyn EmulatorPluginInstance>>,
        viewer: ViewerOptions,
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

        let mut candidates = Vec::new();
        let mut descriptors = Vec::new();
        let mut errors = Vec::new();
        for plugin in plugins {
            match describe_emulator(plugin.as_ref(), &detected.platform, &viewer) {
                Ok(descriptor) => {
                    descriptors.push(descriptor);
                    candidates.push(PreparedEmulator { plugin });
                }
                Err(error) => errors.push(format!("{}: {error}", plugin.id())),
            }
        }
        if candidates.is_empty() {
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
                generation,
                path: detected.path.to_string_lossy().into_owned(),
                name,
                platform: detected.platform,
                selected_emulator,
                candidates: descriptors,
            },
            candidates,
            viewer,
            path: detected.path,
        })
    }

    pub fn descriptor(&self) -> &GameDescriptor {
        &self.descriptor
    }

    pub fn start(&self, request: LauncherStartRequest) -> Result<ConfiguredGameLaunch, String> {
        if request.generation != self.descriptor.generation {
            return Err("This game selection has been replaced by a newer one.".to_owned());
        }
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
        let plugin = self.candidates[candidate_index].plugin.clone();
        let descriptor = &self.descriptor.candidates[candidate_index];
        let (assignments, gamepads) = validate_assignments(descriptor, request.groups)?;
        let mut configuration = plugin
            .materialize(&self.descriptor.platform, assignments)
            .map_err(|error| error.to_string())?;
        apply_settings(&mut configuration, request.settings)?;
        let mut viewer = self.viewer.clone();
        viewer.machine.clone_from(&self.descriptor.platform);
        viewer.gamepads = gamepads;
        Ok(ConfiguredGameLaunch {
            plugin,
            game: self.path.clone(),
            viewer,
            configuration,
        })
    }
}

fn describe_emulator(
    plugin: &dyn EmulatorPluginInstance,
    platform: &str,
    viewer: &ViewerOptions,
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
    viewer: &ViewerOptions,
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
    gamepads: &[GamepadArgument],
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
) -> Result<(PortAssignments, Vec<GamepadArgument>), String> {
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
            gamepads.push(GamepadArgument {
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

fn apply_settings(
    configuration: &mut ConfigurationInstance,
    settings: BTreeMap<String, serde_json::Value>,
) -> Result<(), String> {
    for (path, json) in settings {
        let setting = configuration
            .setting(&path)
            .ok_or_else(|| format!("Emulator option '{path}' is not available."))?;
        let option = configuration.definition().option(setting.option);
        let value = match &option.definition.value.kind {
            ValueKind::Boolean => json
                .as_bool()
                .map(Value::Boolean)
                .ok_or_else(|| format!("'{path}' expects a boolean."))?,
            ValueKind::Integer { range } => {
                let value = json
                    .as_i64()
                    .ok_or_else(|| format!("'{path}' expects an integer."))?;
                if let Some(OptionRange::Integer { min, max }) = range
                    && !(*min..=*max).contains(&value)
                {
                    return Err(format!("'{path}' must be between {min} and {max}."));
                }
                Value::Integer(value)
            }
            ValueKind::Float { range } => {
                let value = json
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| format!("'{path}' expects a finite number."))?;
                if let Some(OptionRange::Float { min, max }) = range
                    && !(*min..=*max).contains(&value)
                {
                    return Err(format!("'{path}' must be between {min} and {max}."));
                }
                Value::Float(value)
            }
            ValueKind::String => Value::String(
                json.as_str()
                    .ok_or_else(|| format!("'{path}' expects text."))?
                    .into(),
            ),
            ValueKind::Path => Value::Path(PathBuf::from(
                json.as_str()
                    .ok_or_else(|| format!("'{path}' expects a path."))?,
            )),
            ValueKind::Select { .. } => {
                let key = json
                    .as_str()
                    .ok_or_else(|| format!("'{path}' expects a choice."))?;
                if !configuration
                    .definition()
                    .choices(setting.option)
                    .is_some_and(|choices| choices.iter().any(|choice| choice.key.as_str() == key))
                {
                    return Err(format!("'{path}' has no choice '{key}'."));
                }
                Value::Choice(key.into())
            }
        };
        *configuration
            .get_mut(&path)
            .expect("validated setting remains present") = value;
    }
    Ok(())
}

fn selector_text(gamepad: &GamepadArgument) -> String {
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

fn consolespec_platform(platform: PlatformIdentifier) -> Option<&'static str> {
    use PlatformIdentifier::*;
    Some(match platform {
        Atari2600 => "ATARI_2600",
        Atari5200 => "ATARI_5200",
        Atari7800 => "ATARI_7800",
        AtariLynx => "ATARI_LYNX",
        AtariJaguar => "ATARI_JAGUAR",
        BandaiWonderSwan => "BANDAI_WS",
        BandaiWonderSwanColor => "BANDAI_WSC",
        BandaiSuFamiTurbo => "BANDAI_SUFAMI_TURBO",
        ColecoVision => "COLECO_CV",
        Intellivision => "MATTEL_INTELLIVISION",
        MagnavoxOdyssey2 => "MAGNAVOX_ODYSSEY2",
        GceVectrex => "GCE_VECTREX",
        MicrosoftXbox => "MICROSOFT_XBOX",
        MicrosoftXbox360 => "MICROSOFT_XBOX360",
        NecPcEngine => "NEC_TG16",
        NintendoGameBoy => "NINTENDO_GB",
        NintendoGameBoyColor => "NINTENDO_GBC",
        NintendoGameBoyAdvance => "NINTENDO_GBA",
        NintendoDS => "NINTENDO_DS",
        NintendoDSi => "NINTENDO_DSI",
        Nintendo3DS => "NINTENDO_3DS",
        NintendoNew3DS => "NINTENDO_NEW_3DS",
        NintendoNES => "NINTENDO_NES",
        NintendoFamicomDiskSystem => "NINTENDO_FDS",
        NintendoSuperNES => "NINTENDO_SNES",
        NintendoSatellaview => "NINTENDO_SATELLAVIEW",
        Nintendo64 => "NINTENDO_N64",
        Nintendo64DD => "NINTENDO_N64DD",
        NintendoVirtualBoy => "NINTENDO_VB",
        NintendoGameCube => "NINTENDO_GCN",
        NintendoWii => "NINTENDO_WII",
        NintendoWiiU => "NINTENDO_WIIU",
        NintendoSwitch => "NINTENDO_SWITCH",
        SegaGameGear => "SEGA_GG",
        SegaMasterSystem => "SEGA_SMS",
        SegaSG1000 => "SEGA_SG1000",
        SegaGenesis => "SEGA_GEN",
        SegaCD => "SEGA_CD",
        Sega32X => "SEGA_32X",
        Sega32XCD => "SEGA_32X_CD",
        SegaSaturn => "SEGA_SAT",
        SegaDreamcast => "SEGA_DC",
        SonyPlayStation => "SONY_PSX",
        SonyPlayStation2 => "SONY_PS2",
        SonyPlayStation3 => "SONY_PS3",
        SonyPlayStationPortable => "SONY_PSP",
        SonyPlayStationVita => "SONY_PSVITA",
        PhilipsCDi => "PHILIPS_CDI",
        SnkNeoGeoPocket => "SNK_NGP",
        SnkNeoGeoPocketColor => "SNK_NGPC",
        SnkNeoGeoCD => "SNK_NGCD",
        ThreeDO => "PANASONIC_3DO",
        WataraSupervision => "WATARA_SUPERVISION",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnected_ports_do_not_consume_requested_input_sources() {
        let gamepads = [GamepadArgument {
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

    #[test]
    fn maps_platform_ids_used_by_the_loaded_plugins() {
        assert_eq!(
            consolespec_platform(PlatformIdentifier::NintendoWiiU),
            Some("NINTENDO_WIIU")
        );
        assert_eq!(
            consolespec_platform(PlatformIdentifier::NintendoGameCube),
            Some("NINTENDO_GCN")
        );
        assert_eq!(
            consolespec_platform(PlatformIdentifier::SonyPlayStation),
            Some("SONY_PSX")
        );
    }
}
