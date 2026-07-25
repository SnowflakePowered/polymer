//! Environment-backed configuration shared by hook and producer crates.

use regex::Regex;
use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
    sync::LazyLock,
};

pub const SESSION: &str = "SNOWGLASS_SESSION";
pub const LAYER_LOG_FILE: &str = "SNOWGLASS_LOG_FILE";
pub const DINPUT_LOG_FILE: &str = "SNOWGLASS_DINPUT_LOG_FILE";
pub const WIN32_LOG_FILE: &str = "SNOWGLASS_WIN32_LOG_FILE";
pub const SDL_LOG_FILE: &str = "SNOWGLASS_SDL_LOG_FILE";
pub const SDL_GAMEPADS: &str = "SNOWGLASS_SDL_GAMEPADS";
pub const STONE_LAYOUT: &str = "SNOWGLASS_STONE_LAYOUT";
pub const STONE_LAYOUTS: &str = "SNOWGLASS_STONE_LAYOUTS";
pub const STONE_DATABASE: &str = "SNOWGLASS_STONE_DATABASE";
pub const DXGI_LOG_FILE: &str = "SNOWGLASS_DXGI_LOG_FILE";
pub const D3D11_LOG_FILE: &str = "SNOWGLASS_D3D11_LOG_FILE";
pub const D3D12_LOG_FILE: &str = "SNOWGLASS_D3D12_LOG_FILE";
pub const OPENGL_LOG_FILE: &str = "SNOWGLASS_OPENGL_LOG_FILE";
pub const GDI_LOG_FILE: &str = "SNOWGLASS_GDI_LOG_FILE";
pub const TRACK_RENDER_TARGET: &str = "SNOWGLASS_TRACK_RENDER_TARGET";
pub const TRACK_RENDER_TARGETS: &str = "SNOWGLASS_TRACK_RENDER_TARGETS";
pub const TRACK_RENDER_TARGET_SURFACE_PATTERNS: &str =
    "SNOWGLASS_TRACK_RENDER_TARGET_SURFACE_PATTERNS";
pub const GRAPH_TRACKING: &str = "SNOWGLASS_GRAPH_TRACKING";
pub const FORCE_KEYED_MUTEX: &str = "SNOWGLASS_FORCE_KEYED_MUTEX";
/// Removal of application-requested display pacing. This is enabled by default;
/// set the variable to `0`, `false`, `off`, or `no` for targets that require
/// their native DXGI/Vulkan present behavior.
pub const PRESENT_UNTHROTTLED: &str = "SNOWGLASS_PRESENT_UNTHROTTLED";

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

/// Fixed virtual-pad topology exposed for the lifetime of the target process.
pub fn sdl_gamepad_count() -> usize {
    std::env::var(SDL_GAMEPADS)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1)
        .clamp(1, u8::MAX as usize + 1)
}

pub fn stone_layout() -> String {
    std::env::var(STONE_LAYOUT).unwrap_or_else(|_| "XBOX_CONTROLLER".to_owned())
}

pub fn stone_layouts() -> Vec<String> {
    std::env::var(STONE_LAYOUTS)
        .ok()
        .map(|layouts| layouts.split(',').map(str::to_owned).collect())
        .filter(|layouts: &Vec<_>| !layouts.is_empty() && layouts.iter().all(|id| !id.is_empty()))
        .unwrap_or_else(|| vec![stone_layout()])
}

pub fn stone_database() -> Option<PathBuf> {
    path(STONE_DATABASE)
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
    is_set(GRAPH_TRACKING)
}

pub fn force_keyed_mutex() -> bool {
    is_set(FORCE_KEYED_MUTEX)
}

pub fn present_unthrottled() -> bool {
    static ENABLED: LazyLock<bool> = LazyLock::new(|| enabled_by_default(PRESENT_UNTHROTTLED));
    *ENABLED
}

fn path(variable: &str) -> Option<PathBuf> {
    std::env::var_os(variable).map(PathBuf::from)
}

fn is_set(variable: &str) -> bool {
    std::env::var_os(variable).is_some()
}

fn enabled_by_default(variable: &str) -> bool {
    enabled_by_default_value(std::env::var_os(variable).as_deref())
}

fn enabled_by_default_value(value: Option<&OsStr>) -> bool {
    value.is_none_or(|value| {
        !matches!(
            value.to_string_lossy().trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
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
    fn default_enabled_values_support_an_explicit_opt_out() {
        assert!(enabled_by_default_value(None));
        assert!(enabled_by_default_value(Some(OsStr::new("1"))));
        assert!(enabled_by_default_value(Some(OsStr::new("yes"))));
        for value in ["0", "false", "OFF", " no "] {
            assert!(!enabled_by_default_value(Some(OsStr::new(value))));
        }
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
