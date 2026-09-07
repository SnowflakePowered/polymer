//! Environment-backed configuration shared by hook and producer crates.

pub use polymer_protocol::{RenderConfiguration, RenderMode};
use std::{path::PathBuf, sync::LazyLock};

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
/// Validate before installing a D3D11, D3D12, Vulkan, or OpenGL presentation hook.
pub fn require_swapchain() -> Result<(), &'static str> {
    match render_mode()? {
        RenderMode::Swapchain => Ok(()),
        _ => Err("D3D11, D3D12, Vulkan, and OpenGL require swapchain mode with fixed surface resolutions"),
    }
}

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

/// Capture must never masquerade as virtual presentation, including
/// when a caller injects hooks directly instead of using the native launcher.
pub fn capture_enabled() -> bool {
    static ENABLED: LazyLock<bool> = LazyLock::new(|| match render_mode() {
        Ok(RenderMode::Intercept) => true,
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

fn path(variable: &str) -> Option<PathBuf> {
    std::env::var_os(variable).map(PathBuf::from)
}
