//! Runtime Stone controller registry and user-remapping documents.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub const EMBEDDED_STONE_JSON: &str = include_str!("../stone/dist/stone.json");
pub const EMBEDDED_GAMECONTROLLER_DB: &str =
    include_str!("../SDL_GameControllerDB/gamecontrollerdb.txt");
pub const EMBEDDED_DEFAULT_KEYBOARD_MAPPING: &str = include_str!("../keyboard/DEFAULT.kb.toml");

#[derive(Debug, Deserialize)]
struct Document {
    #[serde(rename = "Controllers")]
    controllers: HashMap<String, ControllerDocument>,
    version: String,
}

#[derive(Debug, Deserialize)]
struct ControllerDocument {
    #[serde(rename = "LayoutID")]
    layout_id: String,
    #[serde(rename = "FriendlyName")]
    friendly_name: String,
    #[serde(rename = "Layout")]
    layout: HashMap<String, ElementDocument>,
}

#[derive(Debug, Deserialize)]
struct ElementDocument {
    #[serde(rename = "Label")]
    label: String,
    #[serde(rename = "Type")]
    kind: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoneElement {
    pub id: String,
    pub label: String,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoneLayout {
    pub id: String,
    pub friendly_name: String,
    pub elements: Vec<StoneElement>,
    #[serde(skip)]
    element_ids: HashSet<String>,
}

impl StoneLayout {
    pub fn has(&self, element: &str) -> bool {
        self.element_ids.contains(element)
    }

    pub fn same_topology(&self, other: &Self) -> bool {
        self.elements.len() == other.elements.len()
            && self
                .elements
                .iter()
                .zip(&other.elements)
                .all(|(left, right)| left.id == right.id && left.kind == right.kind)
    }

    /// Capabilities for a standard SDL virtual gamepad projection. Stone
    /// elements without an SDL gamepad equivalent remain unavailable through
    /// this output target.
    pub fn sdl_output(&self) -> SdlOutputLayout {
        let mut button_mask = 0u32;
        for (element, index) in [
            ("ButtonA", 0),
            ("ButtonB", 1),
            ("ButtonX", 2),
            ("ButtonY", 3),
            ("ButtonSelect", 4),
            ("ButtonGuide", 5),
            ("ButtonStart", 6),
            ("ButtonClickL", 7),
            ("ButtonClickR", 8),
            ("ButtonL", 9),
            ("ButtonR", 10),
            ("DirectionalN", 11),
            ("DirectionalS", 12),
            ("DirectionalW", 13),
            ("DirectionalE", 14),
            ("Button0", 15),
        ] {
            if self.has(element) {
                button_mask |= 1 << index;
            }
        }
        let mut axis_mask = 0u32;
        if self.has("AxisLeftAnalogPositiveX") || self.has("AxisLeftAnalogNegativeX") {
            axis_mask |= 1 << 0;
        }
        if self.has("AxisLeftAnalogPositiveY") || self.has("AxisLeftAnalogNegativeY") {
            axis_mask |= 1 << 1;
        }
        if self.has("AxisRightAnalogPositiveX") || self.has("AxisRightAnalogNegativeX") {
            axis_mask |= 1 << 2;
        }
        if self.has("AxisRightAnalogPositiveY") || self.has("AxisRightAnalogNegativeY") {
            axis_mask |= 1 << 3;
        }
        if self.has("TriggerLeft") {
            axis_mask |= 1 << 4;
        }
        if self.has("TriggerRight") {
            axis_mask |= 1 << 5;
        }
        // Pointer X/Y are Snowglass-specific raw joystick axes and are always
        // present, independently of the selected Stone controller topology.
        axis_mask |= (1 << 6) | (1 << 7);
        SdlOutputLayout {
            button_mask,
            axis_mask,
            accelerometer: self.has("AccelerometerAxisPositiveX")
                || self.has("AccelerometerAxisNegativeX"),
            gyroscope: self.has("GyroscopeAxisPositiveX") || self.has("GyroscopeAxisNegativeX"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SdlOutputLayout {
    pub button_mask: u32,
    pub axis_mask: u32,
    pub accelerometer: bool,
    pub gyroscope: bool,
}

#[derive(Clone, Debug)]
pub struct StoneRegistry {
    pub version: String,
    layouts: HashMap<String, StoneLayout>,
}

impl StoneRegistry {
    pub fn from_json(json: &str) -> Result<Self, String> {
        let document: Document =
            serde_json::from_str(json).map_err(|error| format!("invalid Stone JSON: {error}"))?;
        let mut layouts = HashMap::with_capacity(document.controllers.len());
        for (key, controller) in document.controllers {
            if key != controller.layout_id {
                return Err(format!(
                    "Stone controller key {key} disagrees with LayoutID {}",
                    controller.layout_id
                ));
            }
            let mut elements = controller
                .layout
                .into_iter()
                .map(|(id, element)| StoneElement {
                    id,
                    label: element.label,
                    kind: element.kind,
                })
                .collect::<Vec<_>>();
            elements.sort_by(|left, right| left.id.cmp(&right.id));
            let element_ids = elements.iter().map(|element| element.id.clone()).collect();
            layouts.insert(
                key,
                StoneLayout {
                    id: controller.layout_id,
                    friendly_name: controller.friendly_name,
                    elements,
                    element_ids,
                },
            );
        }
        Ok(Self {
            version: document.version,
            layouts,
        })
    }

    pub fn layout(&self, id: &str) -> Option<&StoneLayout> {
        self.layouts.get(id)
    }

    pub fn layouts(&self) -> impl Iterator<Item = &StoneLayout> {
        self.layouts.values()
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RemapDatabase {
    #[serde(default)]
    pub profiles: Vec<RemapProfile>,
    /// Convenience bindings applied to every matching controller.
    #[serde(default)]
    pub bindings: HashMap<String, InputBinding>,
}

impl RemapDatabase {
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|error| format!("invalid remapping JSON: {error}"))
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RemapProfile {
    pub guid: Option<String>,
    pub vendor: Option<u16>,
    pub product: Option<u16>,
    pub layout: Option<String>,
    #[serde(default)]
    pub bindings: HashMap<String, InputBinding>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase", untagged)]
pub enum InputBinding {
    Button {
        button: SdlButton,
    },
    Axis {
        axis: SdlAxis,
        #[serde(default)]
        invert: bool,
    },
    Disabled {
        disabled: bool,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SdlButton {
    South,
    East,
    West,
    North,
    Back,
    Guide,
    Start,
    LeftStick,
    RightStick,
    LeftShoulder,
    RightShoulder,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    Misc1,
    RightPaddle1,
    LeftPaddle1,
    RightPaddle2,
    LeftPaddle2,
    Touchpad,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SdlAxis {
    LeftX,
    LeftY,
    RightX,
    RightY,
    LeftTrigger,
    RightTrigger,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct KeyboardMapping {
    #[serde(default)]
    pub bindings: HashMap<String, String>,
}

impl KeyboardMapping {
    pub fn from_toml(toml: &str) -> Result<Self, String> {
        toml::from_str(toml).map_err(|error| format!("invalid keyboard mapping TOML: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_embedded_registry() {
        let registry = StoneRegistry::from_json(EMBEDDED_STONE_JSON).unwrap();
        let xbox = registry.layout("XBOX_CONTROLLER").unwrap();
        assert!(xbox.has("ButtonA"));
        assert!(xbox.has("AxisLeftAnalogPositiveX"));
        assert!(!registry.version.is_empty());
    }

    #[test]
    fn parses_target_oriented_remap() {
        let remap = RemapDatabase::from_json(
            r#"{"bindings":{"ButtonA":{"button":"east"},"TriggerLeft":{"axis":"right_trigger","invert":false}}}"#,
        )
        .unwrap();
        assert!(matches!(
            remap.bindings["ButtonA"],
            InputBinding::Button {
                button: SdlButton::East
            }
        ));
    }

    #[test]
    fn compiles_sdl_capabilities_from_layout_elements() {
        let registry = StoneRegistry::from_json(EMBEDDED_STONE_JSON).unwrap();
        let switch = registry
            .layout("SWITCH_PRO_CONTROLLER")
            .unwrap()
            .sdl_output();
        assert_ne!(switch.button_mask & (1 << 0), 0); // A / south projection
        assert_ne!(switch.button_mask & (1 << 1), 0); // B / east projection
        assert_ne!(switch.axis_mask & (1 << 0), 0); // left X
        assert_ne!(switch.axis_mask & (1 << 5), 0); // right trigger
        assert!(switch.accelerometer);
        assert!(switch.gyroscope);
    }

    #[test]
    fn parses_embedded_keyboard_mapping() {
        let mapping = KeyboardMapping::from_toml(EMBEDDED_DEFAULT_KEYBOARD_MAPPING).unwrap();
        assert_eq!(
            mapping.bindings.get("ButtonA").map(String::as_str),
            Some("KeyX")
        );
        assert_eq!(
            mapping.bindings.get("DirectionalN").map(String::as_str),
            Some("ArrowUp")
        );
    }
}
