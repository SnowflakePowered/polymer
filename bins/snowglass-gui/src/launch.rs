use snowglass_controllers::ControllerTopology;
pub use snowglass_runtime::Hooks;
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
    pub hooks: Hooks,
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
        if self.hooks.graphics.is_empty() {
            return Err("at least one graphics hook is required".to_owned());
        }
        if self.hooks.graphics.iter().collect::<HashSet<_>>().len() != self.hooks.graphics.len() {
            return Err("graphics hooks must be unique".to_owned());
        }
        if self.hooks.input.iter().collect::<HashSet<_>>().len() != self.hooks.input.len() {
            return Err("input hooks must be unique".to_owned());
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
        self.controller_topology
            .validate_for_machine(&self.machine)?;
        Ok(())
    }
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
                "hooks": {
                    "graphics": ["d3d12"],
                    "input": ["sdl3"]
                },
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
