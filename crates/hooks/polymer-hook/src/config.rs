//! Environment-backed configuration shared by hook and producer crates.

pub use polymer_protocol::RenderConfiguration;
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
pub const BLUETOOTH_API_LOG_FILE: &str = "POLYMER_BLUETOOTH_API_LOG_FILE";
pub const XINPUT_LOG_FILE: &str = "POLYMER_XINPUT_LOG_FILE";
pub const RENDER_CONFIGURATION: &str = "POLYMER_RENDER_CONFIGURATION";

pub const LOG_FILES: [(&str, &str); 10] = [
    (LAYER_LOG_FILE, "polymer-layer.log"),
    (DINPUT_LOG_FILE, "polymer-dinput.log"),
    (WIN32_LOG_FILE, "polymer-win32.log"),
    (SDL_LOG_FILE, "polymer-sdl.log"),
    (DXGI_LOG_FILE, "polymer-dxgi.log"),
    (D3D11_LOG_FILE, "polymer-d3d11.log"),
    (D3D12_LOG_FILE, "polymer-d3d12.log"),
    (OPENGL_LOG_FILE, "polymer-opengl.log"),
    (BLUETOOTH_API_LOG_FILE, "polymer-bluetooth-api.log"),
    (XINPUT_LOG_FILE, "polymer-xinput.log"),
];

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

/// Validate before installing a D3D11, D3D12, Vulkan, or OpenGL presentation hook.
pub fn require_swapchain() -> Result<(), &'static str> {
    render_configuration().map(|_| ())
}

pub fn session() -> Result<String, std::env::VarError> {
    std::env::var(SESSION)
}

pub fn log_file(variable: &str) -> Option<PathBuf> {
    std::env::var_os(variable).map(PathBuf::from)
}
