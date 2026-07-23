//! Environment-backed configuration shared by hook and producer crates.

use std::{ffi::OsString, path::PathBuf};

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
pub const GRAPH_TRACKING: &str = "SNOWGLASS_GRAPH_TRACKING";
pub const FORCE_KEYED_MUTEX: &str = "SNOWGLASS_FORCE_KEYED_MUTEX";

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

pub fn graph_tracking_enabled() -> bool {
    is_set(GRAPH_TRACKING)
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
}
