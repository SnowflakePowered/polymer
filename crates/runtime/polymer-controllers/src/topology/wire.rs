use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputTopology {
    pub revision: u32,
    pub machine: Option<String>,
    pub ports: Vec<PortBinding>,
    pub raw_hid: Vec<RawHidBinding>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortBinding {
    pub player: u8,
    #[serde(default)]
    pub keyboard_mouse: bool,
    pub group: String,
    pub port: u8,
    pub layout: String,
    pub mode: RemapMode,
    pub bindings: Vec<(String, Option<String>)>,
    #[serde(default)]
    pub orientation: polymer_config::input::Orientation,
    #[serde(default)]
    pub pointer: PointerBinding,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RemapMode {
    #[default]
    Position,
    Name,
    Custom,
    Passthrough,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PointerBinding {
    pub source: PointerSource,
    pub mode: PointerMode,
}

impl Default for PointerBinding {
    fn default() -> Self {
        Self {
            source: PointerSource::DeviceThenHostMouse,
            mode: PointerMode::Relative,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PointerSource {
    None,
    DeviceNative,
    HostMouse,
    #[default]
    DeviceThenHostMouse,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PointerMode {
    None,
    #[default]
    Relative,
    AbsoluteDevice,
    AbsoluteScreen,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawHidBinding {
    pub player: u8,
    pub port: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub pipe: String,
}
