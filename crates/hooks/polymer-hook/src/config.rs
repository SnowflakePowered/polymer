//! Environment-backed configuration shared by hook and producer crates.

pub use polymer_presentation_config::{RenderConfiguration, RenderMode};
use regex::Regex;
use std::{ffi::OsString, path::PathBuf, sync::LazyLock};

pub const SESSION: &str = "POLYMER_SESSION";
pub const LAYER_LOG_FILE: &str = "POLYMER_LOG_FILE";
pub const DINPUT_LOG_FILE: &str = "POLYMER_DINPUT_LOG_FILE";
pub const WIN32_LOG_FILE: &str = "POLYMER_WIN32_LOG_FILE";
pub const SDL_LOG_FILE: &str = "POLYMER_SDL_LOG_FILE";
pub const DXGI_LOG_FILE: &str = "POLYMER_DXGI_LOG_FILE";
pub const D3D11_LOG_FILE: &str = "POLYMER_D3D11_LOG_FILE";
pub const D3D12_LOG_FILE: &str = "POLYMER_D3D12_LOG_FILE";
pub const OPENGL_LOG_FILE: &str = "POLYMER_OPENGL_LOG_FILE";
pub const GDI_LOG_FILE: &str = "POLYMER_GDI_LOG_FILE";
pub const BLUETOOTH_API_LOG_FILE: &str = "POLYMER_BLUETOOTH_API_LOG_FILE";
pub const TRACK_RENDER_TARGET: &str = "POLYMER_TRACK_RENDER_TARGET";
pub const TRACK_RENDER_TARGETS: &str = "POLYMER_TRACK_RENDER_TARGETS";
pub const TRACK_RENDER_TARGET_SURFACE_PATTERNS: &str =
    "POLYMER_TRACK_RENDER_TARGET_SURFACE_PATTERNS";
pub const RENDER_CONFIGURATION: &str = "POLYMER_RENDER_CONFIGURATION";

static RENDER: LazyLock<Result<RenderConfiguration, String>> = LazyLock::new(|| {
    let payload = std::env::var(RENDER_CONFIGURATION)
        .map_err(|error| format!("{RENDER_CONFIGURATION} is required: {error}"))?;
    let configuration: RenderConfiguration = serde_json::from_str(&payload)
        .map_err(|error| format!("invalid {RENDER_CONFIGURATION}: {error}"))?;
    configuration.validate()?;
    Ok(configuration)
});

pub fn render_configuration() -> Result<&'static RenderConfiguration, &'static str> {
    RENDER.as_ref().map_err(String::as_str)
}

pub fn render_mode() -> Result<RenderMode, &'static str> {
    render_configuration().map(|configuration| configuration.render_mode)
}
/// Validate before installing a D3D11, D3D12, or Vulkan presentation hook.
pub fn require_swapchain() -> Result<(), &'static str> {
    match render_mode()? {
        RenderMode::Swapchain => Ok(()),
        _ => Err("D3D11, D3D12, and Vulkan require swapchain mode with fixed surface resolutions"),
    }
}

pub const FORCE_KEYED_MUTEX: &str = "POLYMER_FORCE_KEYED_MUTEX";

pub fn session() -> Result<String, std::env::VarError> {
    std::env::var(SESSION)
}

pub fn layer_log_file() -> Option<PathBuf> {
    path(LAYER_LOG_FILE)
}

pub fn dinput_log_file() -> Option<PathBuf> {
    path(DINPUT_LOG_FILE)
}

pub fn win32_log_file() -> Option<PathBuf> {
    path(WIN32_LOG_FILE)
}

pub fn sdl_log_file() -> Option<PathBuf> {
    path(SDL_LOG_FILE)
}

pub fn dxgi_log_file() -> Option<PathBuf> {
    path(DXGI_LOG_FILE)
}

pub fn d3d11_log_file() -> Option<PathBuf> {
    path(D3D11_LOG_FILE)
}

pub fn d3d12_log_file() -> Option<PathBuf> {
    path(D3D12_LOG_FILE)
}

pub fn opengl_log_file() -> Option<PathBuf> {
    path(OPENGL_LOG_FILE)
}

pub fn gdi_log_file() -> Option<PathBuf> {
    path(GDI_LOG_FILE)
}

pub fn bluetooth_api_log_file() -> Option<PathBuf> {
    path(BLUETOOTH_API_LOG_FILE)
}

pub fn tracking_enabled() -> bool {
    is_set(TRACK_RENDER_TARGET)
}

/// The render-target indices to capture, in the caller's requested order.
pub fn tracked_render_target_indices() -> Option<Vec<usize>> {
    match std::env::var_os(TRACK_RENDER_TARGETS) {
        Some(value) => parse_render_target_indices(value),
        None => tracking_enabled().then(|| vec![0]),
    }
}

/// How many selection entries the requested indices reach into.
pub fn tracked_render_target_count() -> Option<usize> {
    tracked_render_target_indices()
        .and_then(|indices| indices.into_iter().max())
        .map(|max| max + 1)
}

static TRACKED_SURFACE_PATTERNS: LazyLock<Vec<Option<Regex>>> = LazyLock::new(|| {
    let count = tracked_render_target_indices().map_or(0, |indices| indices.len());
    std::env::var_os(TRACK_RENDER_TARGET_SURFACE_PATTERNS)
        .map(|value| parse_surface_patterns(value, count))
        .unwrap_or_default()
});

/// Whether tracking request `slot` has a surface-title regex, and if so
/// whether it matches `title`. Slots are in CLI/display order.
pub fn tracked_render_target_surface_matches(slot: usize, title: &str) -> Option<bool> {
    TRACKED_SURFACE_PATTERNS
        .get(slot)?
        .as_ref()
        .map(|pattern| pattern.is_match(title))
}

pub fn tracked_render_target_has_surface_pattern(slot: usize) -> bool {
    TRACKED_SURFACE_PATTERNS
        .get(slot)
        .is_some_and(Option::is_some)
}

pub fn tracked_render_target_uses_surface_patterns() -> bool {
    TRACKED_SURFACE_PATTERNS.iter().any(Option::is_some)
}

/// Assign live presentation surfaces to tracking request slots. Regex slots
/// claim matching titles first; unqualified slots then take the remaining
/// surfaces in the caller's stable order.
pub fn tracked_render_target_surface_slots<T: Copy + Eq>(
    surfaces: &[T],
    mut title: impl FnMut(T) -> Option<String>,
) -> Vec<Option<T>> {
    assign_surface_slots(
        tracked_render_target_indices().map_or(0, |indices| indices.len()),
        surfaces,
        |slot| tracked_render_target_has_surface_pattern(slot),
        |slot, surface| {
            title(surface).is_some_and(|title| {
                tracked_render_target_surface_matches(slot, &title) == Some(true)
            })
        },
    )
}

pub fn graph_tracking_enabled() -> bool {
    render_mode() == Ok(RenderMode::GraphTracking)
}

/// Legacy capture must never masquerade as virtual presentation, including
/// when a caller injects hooks directly instead of using the native launcher.
pub fn capture_enabled() -> bool {
    static ENABLED: LazyLock<bool> = LazyLock::new(|| match render_mode() {
        Ok(RenderMode::Intercept | RenderMode::GraphTracking) => true,
        Ok(RenderMode::Swapchain) => {
            tracing::error!("capture is disabled in swapchain mode");
            false
        }
        Err(error) => {
            tracing::error!(%error, "invalid render configuration; native capture is disabled");
            false
        }
    });
    *ENABLED
}

pub fn force_keyed_mutex() -> bool {
    is_set(FORCE_KEYED_MUTEX)
}

fn path(variable: &str) -> Option<PathBuf> {
    std::env::var_os(variable).map(PathBuf::from)
}

fn is_set(variable: &str) -> bool {
    std::env::var_os(variable).is_some()
}

fn parse_render_target_indices(value: OsString) -> Option<Vec<usize>> {
    let indices = value
        .to_str()?
        .split(',')
        .map(|index| index.trim().parse::<usize>().ok())
        .collect::<Option<Vec<_>>>()?;
    (!indices.is_empty()).then_some(indices)
}

fn parse_surface_patterns(value: OsString, count: usize) -> Vec<Option<Regex>> {
    let Some(value) = value.to_str() else {
        return Vec::new();
    };
    let Ok(mut patterns) = serde_json::from_str::<Vec<Option<String>>>(value) else {
        return Vec::new();
    };
    patterns.resize(count, None);
    patterns.truncate(count);
    patterns
        .into_iter()
        .map(|pattern| pattern.and_then(|pattern| Regex::new(&pattern).ok()))
        .collect()
}

fn assign_surface_slots<T: Copy + Eq>(
    count: usize,
    surfaces: &[T],
    mut has_pattern: impl FnMut(usize) -> bool,
    mut matches: impl FnMut(usize, T) -> bool,
) -> Vec<Option<T>> {
    let mut slots = vec![None; count];
    let mut used = Vec::new();
    for (slot, assigned) in slots.iter_mut().enumerate() {
        if !has_pattern(slot) {
            continue;
        }
        if let Some(&surface) = surfaces
            .iter()
            .find(|&&surface| !used.contains(&surface) && matches(slot, surface))
        {
            *assigned = Some(surface);
            used.push(surface);
        }
    }
    for (slot, assigned) in slots.iter_mut().enumerate() {
        if assigned.is_some() || has_pattern(slot) {
            continue;
        }
        if let Some(&surface) = surfaces.iter().find(|&&surface| !used.contains(&surface)) {
            *assigned = Some(surface);
            used.push(surface);
        }
    }
    slots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_render_target_indices() {
        assert_eq!(
            parse_render_target_indices(OsString::from("2, 0,1")),
            Some(vec![2, 0, 1])
        );
        assert_eq!(parse_render_target_indices(OsString::from("")), None);
        assert_eq!(parse_render_target_indices(OsString::from("0,nope")), None);
    }

    #[test]
    fn parses_surface_patterns_in_request_order() {
        let patterns = parse_surface_patterns(
            OsString::from(r#"[null,"(?i)gamepad view","Main: [0-9]+"]"#),
            3,
        );
        assert!(patterns[0].is_none());
        assert!(patterns[1].as_ref().unwrap().is_match("GamePad View"));
        assert!(patterns[2].as_ref().unwrap().is_match("Main: 42"));
        assert!(parse_surface_patterns(OsString::from("[\"(\" ]"), 1)[0].is_none());
    }

    #[test]
    fn regex_slots_claim_surfaces_before_unqualified_slots() {
        let slots = assign_surface_slots(
            2,
            &[10, 20],
            |slot| slot == 0,
            |slot, surface| slot == 0 && surface == 20,
        );
        assert_eq!(slots, [Some(20), Some(10)]);
    }

    #[test]
    fn surface_assignment_never_exceeds_the_request_count() {
        let slots = assign_surface_slots(1, &[10, 20], |_| false, |_, _| false);
        assert_eq!(slots, [Some(10)]);
    }
}
