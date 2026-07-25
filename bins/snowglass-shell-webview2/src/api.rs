use snowglass_input::{
    provider::sdl3::DeviceSelector,
    stone::{InputBinding, SdlAxis, SdlButton},
};
use snowglass_renderer::cli::{GamepadArgument, HookBackend, ViewerOptions};
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
};

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchOptions {
    pub command: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub isolated: bool,
    pub hook: Vec<GraphicsHook>,
    pub surfaces: Vec<TrackedSurface>,
    #[serde(default)]
    pub gamepad_mapping: Vec<GamepadMapping>,
    pub webrtc_bind: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsHook {
    Opengl,
    Vulkan,
    D3d11,
    D3d12,
}

impl From<GraphicsHook> for HookBackend {
    fn from(value: GraphicsHook) -> Self {
        match value {
            GraphicsHook::Opengl => Self::OpenGl,
            GraphicsHook::Vulkan => Self::Vulkan,
            GraphicsHook::D3d11 => Self::D3d11,
            GraphicsHook::D3d12 => Self::D3d12,
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackedSurface {
    pub index: u32,
    #[serde(default)]
    pub rotation: i32,
    pub selector: Option<String>,
    /// Stable DOM attachment identifier assigned by the JavaScript wrapper.
    pub attach: String,
    #[serde(default)]
    pub webrtc: bool,
}

impl TrackedSurface {
    pub fn quarter_turns(&self) -> u8 {
        self.rotation.rem_euclid(4) as u8
    }
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GamepadMapping {
    pub gamepad_id: GamepadId,
    #[serde(default)]
    pub mouse: bool,
    pub slot: usize,
    pub layout: String,
    #[serde(default)]
    pub mapping: HashMap<String, String>,
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
pub enum GamepadId {
    Index(usize),
    Name(String),
}

impl GamepadMapping {
    pub fn selector(&self) -> DeviceSelector {
        match &self.gamepad_id {
            GamepadId::Index(index) => DeviceSelector::Index(*index),
            GamepadId::Name(value) if value == "K" => DeviceSelector::Keyboard,
            GamepadId::Name(value) => {
                let compact = value.replace('-', "");
                if compact.len() == 32 && compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    DeviceSelector::Guid(compact.to_ascii_lowercase())
                } else {
                    DeviceSelector::Name(value.clone())
                }
            }
        }
    }
}

impl LaunchOptions {
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
            if surface.attach.is_empty() {
                return Err("surface attachment IDs must be non-empty".to_owned());
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
        for mapping in &self.gamepad_mapping {
            for source in mapping.mapping.values() {
                parse_input_binding(source)?;
            }
        }
        Ok(())
    }

    pub fn target(&self) -> Vec<String> {
        std::iter::once(self.command.clone())
            .chain(self.arguments.clone())
            .collect()
    }

    pub fn webrtc_bind(&self) -> Result<SocketAddr, String> {
        self.webrtc_bind
            .as_deref()
            .unwrap_or("127.0.0.1:8080")
            .parse()
            .map_err(|error| format!("invalid WebRTC bind address: {error}"))
    }

    pub fn from_cli(options: &ViewerOptions) -> Result<Option<Self>, String> {
        let Some(command) = options.target.first() else {
            return Ok(None);
        };
        let hook = options
            .hooks
            .iter()
            .map(|hook| match hook {
                HookBackend::OpenGl => Ok(GraphicsHook::Opengl),
                HookBackend::Vulkan => Ok(GraphicsHook::Vulkan),
                HookBackend::D3d11 => Ok(GraphicsHook::D3d11),
                HookBackend::D3d12 => Ok(GraphicsHook::D3d12),
                HookBackend::Gdi => Err("the WebView2 GameView API does not expose GDI".to_owned()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let tracked = options.track_render_targets.clone().unwrap_or_else(|| {
            vec![snowglass_renderer::cli::TrackedRenderTarget {
                index: 0,
                quarter_turns: 0,
                surface_pattern: None,
            }]
        });
        let webrtc = options.webrtc.as_deref().unwrap_or_default();
        let surfaces = tracked
            .into_iter()
            .map(|surface| TrackedSurface {
                index: surface.index,
                rotation: i32::from(surface.quarter_turns),
                selector: surface.surface_pattern,
                attach: format!("cli-game-view-target-{}", surface.index),
                webrtc: webrtc.contains(&surface.index),
            })
            .collect();
        let launch = Self {
            command: command.clone(),
            arguments: options.target[1..].to_vec(),
            isolated: options.isolate,
            hook,
            surfaces,
            gamepad_mapping: options.gamepads.iter().map(GamepadMapping::from).collect(),
            webrtc_bind: options
                .webrtc
                .as_ref()
                .map(|_| options.bind_webrtc.to_string()),
        };
        launch.validate()?;
        Ok(Some(launch))
    }
}

impl From<&GamepadArgument> for GamepadMapping {
    fn from(value: &GamepadArgument) -> Self {
        let gamepad_id = match &value.selector {
            DeviceSelector::Keyboard => GamepadId::Name("K".to_owned()),
            DeviceSelector::Index(index) => GamepadId::Index(*index),
            DeviceSelector::Name(name) | DeviceSelector::Guid(name) => {
                GamepadId::Name(name.clone())
            }
        };
        Self {
            gamepad_id,
            mouse: value.force_mouse,
            slot: value.slot,
            layout: value.layout.clone(),
            mapping: HashMap::new(),
        }
    }
}

pub fn parse_input_binding(value: &str) -> Result<InputBinding, String> {
    let value = value.trim().to_ascii_lowercase();
    if value == "disabled" || value == "none" {
        return Ok(InputBinding::Disabled { disabled: true });
    }
    let (kind, mut source) = value
        .split_once(':')
        .map_or(("button", value.as_str()), |parts| parts);
    let invert = source.starts_with('-');
    source = source.trim_start_matches(['+', '-']);
    match kind {
        "button" => {
            serde_json::from_value::<SdlButton>(serde_json::Value::String(source.to_owned()))
                .map(|button| InputBinding::Button { button })
                .map_err(|error| format!("invalid gamepad button binding '{value}': {error}"))
        }
        "axis" => serde_json::from_value::<SdlAxis>(serde_json::Value::String(source.to_owned()))
            .map(|axis| InputBinding::Axis { axis, invert })
            .map_err(|error| format!("invalid gamepad axis binding '{value}': {error}")),
        _ => Err(format!(
            "invalid mapping '{value}'; expected button:NAME, axis:NAME, or disabled"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_frontend_launch_options() {
        let launch: LaunchOptions = serde_json::from_str(
            r#"{
                "command": "game.exe",
                "arguments": ["--fullscreen"],
                "isolated": true,
                "hook": ["d3d12"],
                "surfaces": [{
                    "index": 0,
                    "rotation": 0,
                    "selector": "Game",
                    "attach": "target-0",
                    "webrtc": false
                }],
                "gamepadMapping": []
            }"#,
        )
        .unwrap();
        launch.validate().unwrap();
    }

    #[test]
    fn parses_mapping_strings() {
        assert!(matches!(
            parse_input_binding("button:south").unwrap(),
            InputBinding::Button {
                button: SdlButton::South
            }
        ));
        assert!(matches!(
            parse_input_binding("axis:-left_y").unwrap(),
            InputBinding::Axis {
                axis: SdlAxis::LeftY,
                invert: true
            }
        ));
    }
}
