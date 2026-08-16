use crate::input::{GamepadMapping, parse_input_binding};
pub use snowglass_pluginhost::{EmulatorLaunchArguments, GraphicsHook};
use snowglass_renderer::cli::ViewerOptions;
use std::{
    collections::{BTreeMap, HashSet},
    net::SocketAddr,
    path::{Component, Path},
};

// Native game-host launch contracts and their shell adapters.

pub fn plugin_launch_options(
    plan: EmulatorLaunchArguments,
    viewer: &ViewerOptions,
) -> Result<UnattachedLaunchPlan, String> {
    let webrtc = viewer.webrtc.as_deref().unwrap_or_default();
    let launch = UnattachedLaunchPlan {
        command: plan.command,
        arguments: plan.argv,
        isolated: plan.isolate,
        hook: plan.hooks,
        surfaces: plan
            .surfaces
            .into_iter()
            .map(|surface| UnattachedTrackedSurface {
                index: surface.index,
                rotation: surface.rotation,
                selector: surface.pattern,
                webrtc: webrtc.contains(&surface.index),
            })
            .collect(),
        gamepad_mapping: viewer.gamepads.iter().map(GamepadMapping::from).collect(),
        machine: viewer.machine.clone(),
        webrtc_bind: viewer
            .webrtc
            .as_ref()
            .map(|_| viewer.bind_webrtc.to_string()),
        generated_configuration: Some(GeneratedConfiguration {
            root: plan.config_root,
            files: plan
                .config
                .into_iter()
                .map(|(path, bytes)| (path, bytes.into_vec()))
                .collect(),
        }),
    };
    launch.validate()?;
    Ok(launch)
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnattachedLaunchPlan {
    pub command: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub isolated: bool,
    pub hook: Vec<GraphicsHook>,
    pub surfaces: Vec<UnattachedTrackedSurface>,
    #[serde(default)]
    pub gamepad_mapping: Vec<GamepadMapping>,
    /// The machinespec whose ports the mappings below name. A port only exists
    /// relative to the machine that declares its group, so this is not optional:
    /// without it nothing could validate the topology the launch installs.
    pub machine: String,
    pub webrtc_bind: Option<String>,
    #[serde(default)]
    pub generated_configuration: Option<GeneratedConfiguration>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratedConfiguration {
    pub root: String,
    pub files: BTreeMap<String, Vec<u8>>,
}

impl GeneratedConfiguration {
    fn validate(&self) -> Result<(), String> {
        if self.root.trim().is_empty() {
            return Err("generated configuration root must not be empty".to_owned());
        }
        for relative in self.files.keys() {
            let relative = Path::new(relative);
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
            {
                return Err(format!(
                    "plugin config path must stay beneath its root: {}",
                    relative.display()
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnattachedTrackedSurface {
    pub index: u32,
    #[serde(default)]
    pub rotation: i32,
    pub selector: Option<String>,
    #[serde(default)]
    pub webrtc: bool,
}

impl UnattachedLaunchPlan {
    pub fn validate(&self) -> Result<(), String> {
        if self.command.trim().is_empty() {
            return Err("command must not be empty".to_owned());
        }
        if self.hook.is_empty() {
            return Err("at least one graphics hook is required".to_owned());
        }
        if self.hook.iter().collect::<HashSet<_>>().len() != self.hook.len() {
            return Err("graphics hooks must be unique".to_owned());
        }
        if self.surfaces.is_empty() || self.surfaces.len() > 16 {
            return Err("surfaces must contain 1..=16 entries".to_owned());
        }
        let mut indices = HashSet::new();
        for surface in &self.surfaces {
            if surface.index >= 16 || !indices.insert(surface.index) {
                return Err(format!(
                    "surface index {} is outside 0..=15 or duplicated",
                    surface.index
                ));
            }
            if let Some(selector) = &surface.selector {
                regex::Regex::new(selector)
                    .map_err(|error| format!("invalid surface selector '{selector}': {error}"))?;
            }
        }
        let mut slots = self
            .gamepad_mapping
            .iter()
            .map(|mapping| mapping.slot)
            .collect::<Vec<_>>();
        slots.sort_unstable();
        if slots
            .iter()
            .enumerate()
            .any(|(expected, slot)| *slot != expected)
        {
            return Err("gamepad slots must be unique and contiguous from zero".to_owned());
        }
        if self.webrtc_bind.is_some() && !self.surfaces.iter().any(|surface| surface.webrtc) {
            return Err("webrtcBind requires at least one WebRTC surface".to_owned());
        }
        if let Some(bind) = &self.webrtc_bind {
            bind.parse::<SocketAddr>()
                .map_err(|error| format!("invalid WebRTC bind address '{bind}': {error}"))?;
        }
        for mapping in &self.gamepad_mapping {
            for source in mapping.mapping.values() {
                parse_input_binding(source)?;
            }
        }
        if let Some(configuration) = &self.generated_configuration {
            configuration.validate()?;
        }
        Ok(())
    }
}

/// Launch plan accepted only after the shell DOM has resolved every
/// plugin surface selector to a live tracked-surface node.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachedLaunchPlan {
    pub command: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub isolated: bool,
    pub hook: Vec<GraphicsHook>,
    pub surfaces: Vec<AttachedTrackedSurface>,
    #[serde(default)]
    pub gamepad_mapping: Vec<GamepadMapping>,
    pub machine: String,
    pub webrtc_bind: Option<String>,
    #[serde(default)]
    pub generated_configuration: Option<GeneratedConfiguration>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachedTrackedSurface {
    pub index: u32,
    #[serde(default)]
    pub rotation: i32,
    pub selector: Option<String>,
    /// Blitz node ID validated by the shell DOM at attachment time.
    pub attach: usize,
    #[serde(default)]
    pub webrtc: bool,
}

impl AttachedLaunchPlan {
    pub fn validate(&self) -> Result<(), String> {
        if self.command.trim().is_empty() {
            return Err("command must not be empty".to_owned());
        }
        if self.hook.is_empty() {
            return Err("at least one graphics hook is required".to_owned());
        }
        if self.hook.iter().collect::<HashSet<_>>().len() != self.hook.len() {
            return Err("graphics hooks must be unique".to_owned());
        }
        if self.surfaces.is_empty() || self.surfaces.len() > 16 {
            return Err("surfaces must contain 1..=16 entries".to_owned());
        }
        let mut indices = HashSet::new();
        for surface in &self.surfaces {
            if surface.index >= 16 || !indices.insert(surface.index) {
                return Err(format!(
                    "surface index {} is outside 0..=15 or duplicated",
                    surface.index
                ));
            }
            if let Some(selector) = &surface.selector {
                regex::Regex::new(selector)
                    .map_err(|error| format!("invalid surface selector '{selector}': {error}"))?;
            }
        }
        let mut slots = self
            .gamepad_mapping
            .iter()
            .map(|mapping| mapping.slot)
            .collect::<Vec<_>>();
        slots.sort_unstable();
        if slots
            .iter()
            .enumerate()
            .any(|(expected, slot)| *slot != expected)
        {
            return Err("gamepad slots must be unique and contiguous from zero".to_owned());
        }
        if self.webrtc_bind.is_some() && !self.surfaces.iter().any(|surface| surface.webrtc) {
            return Err("webrtcBind requires at least one WebRTC surface".to_owned());
        }
        if let Some(bind) = &self.webrtc_bind {
            bind.parse::<SocketAddr>()
                .map_err(|error| format!("invalid WebRTC bind address '{bind}': {error}"))?;
        }
        for mapping in &self.gamepad_mapping {
            for source in mapping.mapping.values() {
                parse_input_binding(source)?;
            }
        }
        if let Some(configuration) = &self.generated_configuration {
            configuration.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snowglass_input::layout::{InputBinding, SdlAxis, SdlButton};
    use std::collections::BTreeMap;

    #[test]
    fn generated_configuration_paths_are_structurally_validated() {
        let valid = GeneratedConfiguration {
            root: "file:///config".to_owned(),
            files: BTreeMap::from([("nested/settings.xml".to_owned(), vec![1, 2, 3])]),
        };
        valid.validate().unwrap();

        let escaping = GeneratedConfiguration {
            root: "file:///config".to_owned(),
            files: BTreeMap::from([("../settings.xml".to_owned(), vec![])]),
        };
        assert!(escaping.validate().is_err());
    }

    #[test]
    fn validates_shell_launch_options() {
        let launch: AttachedLaunchPlan = serde_json::from_str(
            r#"{
                "command": "game.exe",
                "arguments": ["--fullscreen"],
                "isolated": true,
                "hook": ["d3d12"],
                "surfaces": [{
                    "index": 0,
                    "rotation": 0,
                    "selector": "Game",
                    "attach": 4,
                    "webrtc": false
                }],
                "gamepadMapping": [],
                "machine": "MICROSOFT_XBOX"
            }"#,
        )
        .unwrap();
        launch.validate().unwrap();
    }

    #[test]
    fn parses_mapping_strings() {
        assert!(matches!(
            parse_input_binding("button:south").unwrap(),
            InputBinding::Button {
                button: SdlButton::South
            }
        ));
        assert!(matches!(
            parse_input_binding("axis:-left_y").unwrap(),
            InputBinding::Axis {
                axis: SdlAxis::LeftY,
                invert: true
            }
        ));
    }
}
