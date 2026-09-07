//! The launch contract shared by hosts and graphics producers.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RenderMode {
    #[default]
    Intercept,
    GraphTracking,
    Swapchain,
}

impl RenderMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Intercept => "intercept",
            Self::GraphTracking => "graphTracking",
            Self::Swapchain => "swapchain",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SurfaceRequest {
    pub index: u32,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub resolution: Option<[u32; 2]>,
    #[serde(default)]
    pub rotation: i32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenderConfiguration {
    #[serde(default)]
    pub render_mode: RenderMode,
    #[serde(
        default,
    )]
    pub surfaces: Vec<SurfaceRequest>,
}

impl RenderConfiguration {
    /// Run before injection and again before constructing a producer registry.
    /// Device and format limits must additionally be checked by the backend.
    pub fn validate(&self) -> Result<(), String> {
        if self.render_mode == RenderMode::Swapchain && self.surfaces.is_empty() {
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
            surface
                .validate_extent(self.render_mode)
                .map_err(|error| format!("surfaces[{ordinal}].resolution: {error}"))?;
        }
        Ok(())
    }
}

impl SurfaceRequest {
    fn validate_extent(&self, mode: RenderMode) -> Result<(), &'static str> {
        let Some([width, height]) = self.resolution else {
            return if mode == RenderMode::Swapchain {
                Err("required in swapchain mode")
            } else {
                Ok(())
            };
        };
        if width == 0 || height == 0 {
            return Err("dimensions must be positive integers");
        }
        if width > i32::MAX as u32
            || height > i32::MAX as u32
            || u64::from(width)
                .checked_mul(u64::from(height))
                .and_then(|pixels| pixels.checked_mul(16))
                .is_none()
        {
            return Err("extent overflows supported image geometry");
        }
        Ok(())
    }
}
