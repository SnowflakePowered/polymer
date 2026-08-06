//! Runtime input-layout registry and user-remapping documents.
//!
//! Layouts come from consolespec's `inputspec` documents, embedded at build
//! time by `build.rs`. An inputspec does not spell its elements out: it declares
//! *clusters* — `axes = 2, alignment = "left"` for a stick, `directions = 4`
//! D-pad — and the element ids expand from them by the rules in [`expand`]. The
//! element vocabulary is the same one the SDL hooks, the hub, and the plugin
//! configuration documents use; it is now derived rather than transcribed.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

include!(concat!(env!("OUT_DIR"), "/inputspec.rs"));

pub const EMBEDDED_GAMECONTROLLER_DB: &str =
    include_str!("../SDL_GameControllerDB/gamecontrollerdb.txt");
pub const EMBEDDED_DEFAULT_KEYBOARD_MAPPING: &str = include_str!("../keyboard/DEFAULT.kb.toml");

#[derive(Debug, Deserialize)]
struct Document {
    input: InputDocument,
}

/// The `[input]` table of an inputspec, in the schema's own shape.
#[derive(Debug, Deserialize)]
struct InputDocument {
    id: String,
    #[serde(default)]
    button: Vec<ButtonDocument>,
    #[serde(default)]
    directional: Vec<DirectionalDocument>,
    #[serde(default)]
    analog: Vec<AnalogDocument>,
    #[serde(default)]
    trigger: Vec<TriggerDocument>,
    #[serde(default)]
    rumble: Vec<RumbleDocument>,
    #[serde(default)]
    pointer: Vec<PointerDocument>,
    #[serde(default)]
    touchscreen: Vec<PlainDocument>,
    #[serde(default)]
    keyboard: Vec<PlainDocument>,
    #[serde(default)]
    microphone: Vec<PlainDocument>,
    #[serde(default)]
    camera: Vec<PlainDocument>,
}

#[derive(Debug, Deserialize)]
struct ButtonDocument {
    label: String,
    element: String,
}

#[derive(Debug, Deserialize)]
struct DirectionalDocument {
    label: Option<String>,
    directions: u8,
    /// Present only where a device has more than one directional cluster, and
    /// then on every one of them: the N64's D-pad and C-buttons, the Virtual
    /// Boy's two D-pads. It is the description's own way of telling them
    /// apart, and it is what the binding key will be keyed on.
    #[allow(dead_code)]
    alignment: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnalogDocument {
    label: Option<String>,
    axes: u8,
    /// `inputspec.tosd` marks this required, but two 1-axis clusters omit it —
    /// the 3DS depth slider and the Famicom microphone's volume knob, neither
    /// of which is left, right, or a motion sensor. Optional here so those
    /// documents load; they are identified by label instead.
    alignment: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TriggerDocument {
    label: Option<String>,
    alignment: String,
}

#[derive(Debug, Deserialize)]
struct RumbleDocument {
    label: Option<String>,
    size: String,
}

#[derive(Debug, Deserialize)]
struct PointerDocument {
    label: Option<String>,
    dimensions: u8,
}

#[derive(Debug, Deserialize)]
struct PlainDocument {
    label: Option<String>,
}

/// A button's semantic element name, capitalized into the element id it forms.
/// Anything else is a numbered button, `"0"` through `"31"`.
fn button_suffix(element: &str) -> Option<String> {
    let named = match element {
        "a" => "A",
        "b" => "B",
        "c" => "C",
        "x" => "X",
        "y" => "Y",
        "z" => "Z",
        "l" => "L",
        "r" => "R",
        "start" => "Start",
        "select" => "Select",
        "guide" => "Guide",
        "clickl" => "ClickL",
        "clickr" => "ClickR",
        digits => {
            return digits
                .parse::<u8>()
                .ok()
                .filter(|index| *index <= 31)
                .filter(|_| digits.chars().all(|character| character.is_ascii_digit()))
                .map(|_| digits.to_owned());
        }
    };
    Some(named.to_owned())
}

/// `"3D Slider"` -> `"3DSlider"`, for ids derived from a cluster label.
fn pascal_case(label: &str) -> String {
    label
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut word = word.to_owned();
            word[..1].make_ascii_uppercase();
            word
        })
        .collect()
}

/// The cardinals, then the intercardinals, as the schema orders them.
const HAT4: [&str; 4] = ["N", "E", "S", "W"];
const HAT8: [&str; 8] = ["N", "E", "S", "W", "NE", "SE", "SW", "NW"];

const AXES: [&str; 3] = ["X", "Y", "Z"];

fn direction_label(direction: &str) -> &'static str {
    match direction {
        "N" => "Up",
        "E" => "Right",
        "S" => "Down",
        "W" => "Left",
        "NE" => "Up Right",
        "SE" => "Down Right",
        "SW" => "Down Left",
        _ => "Up Left",
    }
}

/// The word an inputspec would have used for one signed axis of a cluster.
/// Sticks and pointers are named for the screen direction they push toward;
/// motion sensors for the rotation or acceleration they measure.
fn axis_label(alignment: &str, axis: &str, positive: bool) -> &'static str {
    match (alignment, axis, positive) {
        ("gyroscope", "X", true) => "Roll Right",
        ("gyroscope", "X", false) => "Roll Left",
        ("gyroscope", "Y", true) => "Pitch Up",
        ("gyroscope", "Y", false) => "Pitch Down",
        ("gyroscope", _, true) => "Yaw Right",
        ("gyroscope", _, false) => "Yaw Left",
        ("accelerometer", "X", true) => "Right",
        ("accelerometer", "X", false) => "Left",
        ("accelerometer", "Y", true) => "Forward",
        ("accelerometer", "Y", false) => "Backward",
        ("accelerometer", _, true) => "Up",
        ("accelerometer", _, false) => "Down",
        (_, "X", true) => "Right",
        (_, "X", false) => "Left",
        (_, "Y", true) => "Up",
        (_, "Y", false) => "Down",
        (_, _, true) => "Forward",
        (_, _, false) => "Backward",
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputElement {
    pub id: String,
    pub label: String,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputLayout {
    pub id: String,
    pub elements: Vec<InputElement>,
    #[serde(skip)]
    element_ids: HashSet<String>,
}

/// Expands an inputspec's clusters into the element vocabulary.
///
/// Two clusters can expand onto the same id — the N64 pad declares two
/// directionals, the DualSense two small rumble motors — so a repeat takes an
/// ordinal suffix (`DirectionalN2`) in declaration order.
fn expand(document: &InputDocument) -> Result<Vec<InputElement>, String> {
    let mut elements: Vec<InputElement> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut push = |id: String, label: String, kind: &str| {
        let id = if seen.insert(id.clone()) {
            id
        } else {
            let mut ordinal = 2;
            loop {
                let candidate = format!("{id}{ordinal}");
                if seen.insert(candidate.clone()) {
                    break candidate;
                }
                ordinal += 1;
            }
        };
        elements.push(InputElement {
            id,
            label,
            kind: kind.to_owned(),
        });
    };

    for button in &document.button {
        let suffix = button_suffix(&button.element)
            .ok_or_else(|| format!("unknown button element `{}`", button.element))?;
        push(
            format!("Button{suffix}"),
            button.label.clone(),
            "Button",
        );
    }
    for directional in &document.directional {
        let directions: &[&str] = match directional.directions {
            4 => &HAT4,
            8 => &HAT8,
            hats => return Err(format!("directional cluster with {hats} directions")),
        };
        let cluster = directional.label.as_deref().unwrap_or("D-Pad");
        for direction in directions {
            push(
                format!("Directional{direction}"),
                format!("{cluster} {}", direction_label(direction)),
                "Directional",
            );
        }
    }
    for analog in &document.analog {
        let alignment = analog.alignment.as_deref().unwrap_or_default();
        // An unaligned cluster has no place in this element vocabulary — there
        // is no id for "the 3DS depth slider". Naming it after its label keeps
        // the document loadable and the id unique; nothing binds these until
        // binding keys move to cluster paths.
        let prefix = match alignment {
            "left" => "AxisLeftAnalog".to_owned(),
            "right" => "AxisRightAnalog".to_owned(),
            "gyroscope" => "GyroscopeAxis".to_owned(),
            "accelerometer" => "AccelerometerAxis".to_owned(),
            "" => match analog.label.as_deref() {
                Some(label) => format!("Axis{}Analog", pascal_case(label)),
                None => return Err("analog cluster with no alignment or label".to_owned()),
            },
            other => return Err(format!("analog alignment `{other}`")),
        };
        if !(1..=3).contains(&analog.axes) {
            return Err(format!("analog cluster with {} axes", analog.axes));
        }
        let cluster = analog.label.clone().unwrap_or_else(|| match alignment {
            "left" => "Left Stick".to_owned(),
            "right" => "Right Stick".to_owned(),
            other => {
                let mut name = other.to_owned();
                name[..1].make_ascii_uppercase();
                name
            }
        });
        for axis in &AXES[..usize::from(analog.axes)] {
            for positive in [true, false] {
                let sign = if positive { "Positive" } else { "Negative" };
                push(
                    format!("{prefix}{sign}{axis}"),
                    format!("{cluster} {}", axis_label(alignment, axis, positive)),
                    if positive { "AxisPositive" } else { "AxisNegative" },
                );
            }
        }
    }
    for trigger in &document.trigger {
        let side = match trigger.alignment.as_str() {
            "left" => "Left",
            "right" => "Right",
            other => return Err(format!("trigger alignment `{other}`")),
        };
        push(
            format!("Trigger{side}"),
            trigger
                .label
                .clone()
                .unwrap_or_else(|| format!("{side} Trigger")),
            "Trigger",
        );
    }
    for rumble in &document.rumble {
        let (size, fallback) = match rumble.size.as_str() {
            "big" => ("Big", "Strong Rumble"),
            "small" => ("Small", "Weak Rumble"),
            other => return Err(format!("rumble size `{other}`")),
        };
        push(
            format!("Rumble{size}"),
            rumble.label.clone().unwrap_or_else(|| fallback.to_owned()),
            "Rumble",
        );
    }
    for pointer in &document.pointer {
        if !(2..=3).contains(&pointer.dimensions) {
            return Err(format!("pointer with {} dimensions", pointer.dimensions));
        }
        let cluster = pointer.label.as_deref().unwrap_or("Pointer");
        // The absolute position, then the relative axis each dimension carries.
        push(
            format!("Pointer{}D", pointer.dimensions),
            cluster.to_owned(),
            "Pointer",
        );
        for axis in &AXES[..usize::from(pointer.dimensions)] {
            for positive in [true, false] {
                let sign = if positive { "Positive" } else { "Negative" };
                push(
                    format!("PointerAxis{sign}{axis}"),
                    format!("{cluster} {}", axis_label("pointer", axis, positive)),
                    if positive { "AxisPositive" } else { "AxisNegative" },
                );
            }
        }
    }
    for (entries, name) in [
        (&document.touchscreen, "Touchscreen"),
        (&document.keyboard, "Keyboard"),
        (&document.microphone, "Microphone"),
        (&document.camera, "Camera"),
    ] {
        for entry in entries {
            push(
                name.to_owned(),
                entry.label.clone().unwrap_or_else(|| name.to_owned()),
                name,
            );
        }
    }

    if elements.is_empty() {
        return Err("no elements".to_owned());
    }
    elements.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(elements)
}

impl InputLayout {
    /// Parses and expands one inputspec document.
    pub fn from_toml(source: &str) -> Result<Self, String> {
        let document: Document =
            toml::from_str(source).map_err(|error| format!("invalid inputspec: {error}"))?;
        let elements = expand(&document.input)
            .map_err(|error| format!("inputspec {}: {error}", document.input.id))?;
        let element_ids = elements.iter().map(|element| element.id.clone()).collect();
        Ok(Self {
            id: document.input.id,
            elements,
            element_ids,
        })
    }

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

    /// Capabilities for a standard SDL virtual gamepad projection. Elements
    /// without an SDL gamepad equivalent remain unavailable through this output
    /// target.
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
        // present, independently of the selected controller topology.
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
pub struct LayoutRegistry {
    layouts: HashMap<String, InputLayout>,
}

impl LayoutRegistry {
    /// The inputspec documents embedded at build time.
    pub fn embedded() -> Result<Self, String> {
        Self::from_documents(
            EMBEDDED_INPUTSPEC
                .iter()
                .map(|(id, source)| (*id, *source)),
        )
    }

    /// Reads a `consolespec/inputspec` directory, for overriding the embedded
    /// set at runtime.
    pub fn from_directory(directory: &Path) -> Result<Self, String> {
        let entries = std::fs::read_dir(directory)
            .map_err(|error| format!("{}: {error}", directory.display()))?;
        let mut documents = Vec::new();
        for entry in entries {
            let path = entry
                .map_err(|error| format!("{}: {error}", directory.display()))?
                .path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("toml") {
                continue;
            }
            let id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| format!("{}: non-UTF8 filename", path.display()))?
                .to_owned();
            let source = std::fs::read_to_string(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            documents.push((id, source));
        }
        if documents.is_empty() {
            return Err(format!("{}: no inputspec documents", directory.display()));
        }
        Self::from_documents(
            documents
                .iter()
                .map(|(id, source)| (id.as_str(), source.as_str())),
        )
    }

    fn from_documents<'a>(
        documents: impl Iterator<Item = (&'a str, &'a str)>,
    ) -> Result<Self, String> {
        let mut layouts = HashMap::new();
        for (id, source) in documents {
            let layout = InputLayout::from_toml(source)?;
            // The filename is how every other document refers to a layout, so a
            // document whose own id disagrees would be unreachable by one name
            // and shadow another by the other.
            if layout.id != id {
                return Err(format!(
                    "inputspec {id} declares id {}",
                    layout.id
                ));
            }
            layouts.insert(layout.id.clone(), layout);
        }
        Ok(Self { layouts })
    }

    pub fn layout(&self, id: &str) -> Option<&InputLayout> {
        self.layouts.get(id)
    }

    pub fn layouts(&self) -> impl Iterator<Item = &InputLayout> {
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
        let registry = LayoutRegistry::embedded().unwrap();
        let xbox = registry.layout("XBOX_CONTROLLER").unwrap();
        assert!(xbox.has("ButtonA"));
        assert!(xbox.has("AxisLeftAnalogPositiveX"));
        assert!(registry.layouts().count() > 1);
    }

    #[test]
    fn expands_clusters_into_elements() {
        let registry = LayoutRegistry::embedded().unwrap();
        let pad = registry.layout("GAMECUBE_CONTROLLER").unwrap();
        // `directions = 4` becomes four directionals, `axes = 2` two signed pairs.
        for element in [
            "DirectionalN",
            "DirectionalE",
            "DirectionalS",
            "DirectionalW",
            "AxisLeftAnalogPositiveX",
            "AxisLeftAnalogNegativeY",
            "AxisRightAnalogPositiveX",
            "TriggerLeft",
            "TriggerRight",
            "RumbleBig",
            "RumbleSmall",
        ] {
            assert!(pad.has(element), "{element} missing from GAMECUBE_CONTROLLER");
        }
        assert!(!pad.has("ButtonClickL"));

        // A pointer contributes an absolute element and its relative axes.
        let remote = registry.layout("WII_REMOTE_CONTROLLER").unwrap();
        assert!(remote.has("Pointer2D"));
        assert!(remote.has("PointerAxisPositiveX"));
        assert!(remote.has("AccelerometerAxisNegativeZ"));
        // `element = "0"` is the button printed "1" on the shell.
        assert!(remote.has("Button0"));
        assert_eq!(
            remote
                .elements
                .iter()
                .find(|element| element.id == "Button0")
                .map(|element| element.label.as_str()),
            Some("1")
        );
    }

    #[test]
    fn repeated_clusters_take_an_ordinal() {
        // The N64 pad declares two directionals: the D-pad and the C-buttons.
        let registry = LayoutRegistry::embedded().unwrap();
        let pad = registry.layout("N64_CONTROLLER").unwrap();
        assert!(pad.has("DirectionalN"));
        assert!(pad.has("DirectionalN2"));
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
        let registry = LayoutRegistry::embedded().unwrap();
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
