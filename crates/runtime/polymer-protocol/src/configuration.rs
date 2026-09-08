//! Graphics presentation configuration shared by hosts and producers.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SurfaceRequest {
    pub index: u32,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub rotation: i32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenderConfiguration {
    #[serde(default)]
    pub surfaces: Vec<SurfaceRequest>,
}

impl RenderConfiguration {
    /// Run before injection and again before constructing a producer registry.
    /// Device and format limits must additionally be checked by the backend.
    pub fn validate(&self) -> Result<(), String> {
        if self.surfaces.is_empty() {
            return Err("swapchain mode requires at least one surfaces entry".into());
        }
        let mut indices = HashSet::new();
        for (ordinal, surface) in self.surfaces.iter().enumerate() {
            if !indices.insert(surface.index) {
                return Err(format!(
                    "surfaces[{ordinal}]: duplicate index {}",
                    surface.index
                ));
            }
            if let Some(pattern) = &surface.pattern {
                Regex::new(pattern)
                    .map_err(|error| format!("surfaces[{ordinal}].pattern: {error}"))?;
            }
        }
        Ok(())
    }
}
