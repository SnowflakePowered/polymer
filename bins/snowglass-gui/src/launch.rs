use crate::input::GamepadMapping;
use snowglass_input::layout::{InputLayout, ProjectionMode};
pub use snowglass_pluginhost::GraphicsHook;
use std::{collections::HashSet, net::SocketAddr};

// Native game-host launch contracts and their shell adapters.

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnattachedLaunchPlan {
    pub command: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub isolated: bool,
    pub hook: Vec<GraphicsHook>,
    /// Enables presentation dependency-graph traversal for every tracked
    /// surface in this process. The hook exposes this as a process-wide mode.
    #[serde(default)]
    pub graph_tracking: bool,
    pub surfaces: Vec<UnattachedTrackedSurface>,
    #[serde(default)]
    pub gamepad_mapping: Vec<GamepadMapping>,
    /// The machinespec whose ports the mappings below name. A port only exists
    /// relative to the machine that declares its group, so this is not optional:
    /// without it nothing could validate the topology the launch installs.
    pub machine: String,
    pub webrtc_bind: Option<String>,
    #[serde(default)]
    pub prepared_filesystem: Option<String>,
    #[serde(default)]
    pub controller_session: Option<String>,
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
        validate_input_mappings(&self.gamepad_mapping)?;
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
    /// Enables presentation dependency-graph traversal for every tracked
    /// surface in this process. The hook exposes this as a process-wide mode.
    #[serde(default)]
    pub graph_tracking: bool,
    pub surfaces: Vec<AttachedTrackedSurface>,
    #[serde(default)]
    pub gamepad_mapping: Vec<GamepadMapping>,
    pub machine: String,
    #[serde(default)]
    pub prepared_filesystem: Option<String>,
    #[serde(default)]
    pub controller_session: Option<String>,
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
        validate_input_mappings(&self.gamepad_mapping)?;
        Ok(())
    }
}

fn validate_input_mappings(mappings: &[GamepadMapping]) -> Result<(), String> {
    for mapping in mappings {
        let layout = InputLayout::get(&mapping.layout)
            .ok_or_else(|| format!("unknown input layout {}", mapping.layout))?;
        let mut targets = HashSet::new();
        for (target, source) in &mapping.mapping {
            let target: consolespec::input::BindingKey = target
                .parse()
                .map_err(|error| format!("invalid input target '{target}': {error}"))?;
            if !layout.has(target) {
                return Err(format!(
                    "input layout {} has no element {target}",
                    layout.id
                ));
            }
            targets.insert(target);
            if source != "none" {
                let source = source
                    .parse()
                    .map_err(|error| format!("invalid input source '{source}': {error}"))?;
                layout.custom_binding(target, source).ok_or_else(|| {
                    format!("input source '{source}' is not a physical gamepad expression")
                })?;
            }
        }
        if mapping.projection_mode == ProjectionMode::Custom {
            let missing = layout
                .custom_binding_targets()
                .filter(|target| !targets.contains(target))
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_shell_launch_options() {
        let launch: AttachedLaunchPlan = serde_json::from_str(
            r#"{
                "command": "game.exe",
                "arguments": ["--fullscreen"],
                "isolated": true,
                "hook": ["d3d12"],
                "graphTracking": true,
                "surfaces": [{
                    "index": 0,
                    "rotation": 0,
                    "selector": "Game",
                    "attach": 4
                }],
                "gamepadMapping": [],
                "machine": "MICROSOFT_XBOX"
            }"#,
        )
        .unwrap();
        assert!(launch.graph_tracking);
        launch.validate().unwrap();
    }
}
