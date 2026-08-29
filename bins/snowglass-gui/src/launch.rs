use snowglass_input::layout::{InputLayout, ProjectionMode};
use snowglass_input::topology::ControllerTopology;
pub use snowglass_pluginhost::GraphicsHook;
use std::collections::HashSet;

// Native game-host launch contracts and their shell adapters.

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
    pub controller_topology: ControllerTopology,
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
            .controller_topology
            .mappings
            .iter()
            .map(|mapping| mapping.gamepad)
            .collect::<Vec<_>>();
        slots.sort_unstable();
        if slots
            .iter()
            .enumerate()
            .any(|(expected, slot)| usize::from(*slot) != expected)
        {
            return Err("gamepad slots must be unique and contiguous from zero".to_owned());
        }
        validate_input_mappings(&self.controller_topology)?;
        Ok(())
    }
}

fn validate_input_mappings(topology: &ControllerTopology) -> Result<(), String> {
    let mut targets = HashSet::new();
    for mapping in &topology.mappings {
        if !targets.insert((&mapping.target.group, mapping.target.port)) {
            return Err(format!(
                "controller target {}:{} is duplicated",
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
                "controllerTopology": { "mappings": [] },
                "machine": "MICROSOFT_XBOX"
            }"#,
        )
        .unwrap();
        assert!(launch.graph_tracking);
        launch.validate().unwrap();
    }
}
