/// A graphics presentation hook installed into the target process.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphicsHook {
    OpenGl,
    Vulkan,
    D3d11,
    D3d12,
    Metal,
}

impl GraphicsHook {
    pub const ALL: [Self; 5] = [Self::Vulkan, Self::OpenGl, Self::D3d12, Self::D3d11, Self::Metal];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Vulkan => "Vulkan",
            Self::OpenGl => "OpenGL",
            Self::D3d12 => "D3D12",
            Self::D3d11 => "D3D11",
            Self::Metal => "Metal",
        }
    }

    pub const fn class(self) -> u8 {
        match self {
            Self::D3d11 => 0,
            Self::D3d12 => 1,
            Self::OpenGl => 2,
            Self::Vulkan => 3,
            Self::Metal => 4,
        }
    }
}

/// An input hook installed into the target process.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputHook {
    Sdl3,
    BluetoothApiWin32,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Hooks {
    pub graphics: Vec<GraphicsHook>,
    pub input: Vec<InputHook>,
}
