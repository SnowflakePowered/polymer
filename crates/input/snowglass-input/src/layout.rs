//! Input layouts and user-remapping documents.
//!
//! Layouts come from the `consolespec` crate, whose build script parses and
//! expands and cross-checks the documents, so there is nothing to load and no
//! registry to own — [`InputLayout::all`] reads the compiled table.
//! An inputspec does not spell its elements out: it declares
//! *clusters* — `axes = 2, alignment = "left"` for a stick, `directions = 4`
//! D-pad — and `consolespec` expands the elements during its build.
//!
//! Each element carries two names. The id (`ButtonA`) is a display handle and
//! depends on what else the device declares. The [`BindingKey`] (`button.a`) is
//! what a binding names: it is built from the cluster's own fields, so it is
//! stable against a device gaining a cluster, and it is the vocabulary the SDL
//! hooks, the hub, the keyboard defaults and the plugin configuration documents
//! share. `consolespec` owns the grammar and its typed vocabulary.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use consolespec::{Alignment, AnalogClass, BindingKey};

use crate::projection::{Projection, Target};
use crate::sdl::{self, SdlSensor, Wire};

pub const EMBEDDED_GAMECONTROLLER_DB: &str =
    include_str!("../SDL_GameControllerDB/gamecontrollerdb.txt");
pub const EMBEDDED_DEFAULT_KEYBOARD_MAPPING: &str = include_str!("../keyboard/DEFAULT.kb.toml");

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputElement {
    pub id: String,
    /// What a binding names. Unlike the id, it is context-free — built from the
    /// cluster's own fields — so adding a cluster to a device never renames an
    /// existing key. Serializes as its dotted string.
    pub key: BindingKey,
    pub label: String,
    pub kind: String,
}

/// One cluster of an inputspec, before expansion.
///
/// The elements are what a binding names; the cluster is what the SDL
/// projection allocates. A cluster is addressed as `<kind>[.<discriminator>]`,
/// which is the key the projection table uses.
#[derive(Clone, Debug)]
pub struct Cluster {
    /// `button`, `directional`, `analog`, `trigger`, `rumble`, `pointer`, …
    pub kind: &'static str,
    /// `analog` only: `stick`, `slider`, `rotary`, `gyroscope`, `accelerometer`.
    pub class: Option<AnalogClass>,
    /// Where this cluster sits, for the kinds whose keys carry an alignment
    /// *and* a component after it. It is what an unqualified key resolves to.
    pub alignment: Option<Alignment>,
    /// The token [`path`](Self::path) appends: the alignment for most kinds, the
    /// element for a `button`, the size-or-side for a `rumble`.
    pub discriminator: Option<String>,
    /// Degrees of freedom for `analog`/`pointer`, directions for
    /// `directional`, and 1 for everything else.
    pub arity: u8,
    /// Ids this cluster expanded into, in component order.
    pub elements: Vec<String>,
}

impl Cluster {
    /// The projection key: `button.z`, `directional.right`, `analog.stick.left`.
    pub fn path(&self) -> String {
        let mut path = self.kind.to_owned();
        for part in [
            self.class.map(AnalogClass::as_str),
            self.discriminator.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            path.push('.');
            path.push_str(part);
        }
        path
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputLayout {
    pub id: String,
    /// The product's name, e.g. "Nintendo GameCube Controller". Falls back to
    /// the id for the handhelds, which carry no `[meta]`.
    pub name: String,
    pub elements: Vec<InputElement>,
    #[serde(skip)]
    pub clusters: Vec<Cluster>,
    /// Every element's binding key, for the membership test bindings do.
    #[serde(skip)]
    keys: HashSet<BindingKey>,
}

impl InputLayout {
    /// Materializes one input layout from consolespec's compiled static records.
    pub fn from_spec(spec: consolespec::InputSpec) -> Self {
        let elements = spec
            .elements()
            .map(|element| InputElement {
                id: element.id().to_owned(),
                key: element.binding(),
                label: element.label().to_owned(),
                kind: element.kind().to_owned(),
            })
            .collect::<Vec<_>>();
        let clusters = spec
            .clusters()
            .map(|cluster| Cluster {
                kind: cluster.kind(),
                class: cluster.class(),
                alignment: cluster.alignment(),
                discriminator: cluster.discriminator().map(str::to_owned),
                arity: cluster.arity(),
                elements: cluster.elements().map(str::to_owned).collect(),
            })
            .collect();
        let keys = elements.iter().map(|element| element.key).collect();
        let name = spec.name().unwrap_or_else(|| spec.id()).to_owned();
        Self {
            id: spec.id().to_owned(),
            name,
            elements,
            clusters,
            keys,
        }
    }

    /// Every layout consolespec declares, by id.
    pub fn all() -> &'static [InputLayout] {
        layouts()
    }

    /// One layout by its inputspec id.
    pub fn get(id: &str) -> Option<&'static InputLayout> {
        layouts().iter().find(|layout| layout.id == id)
    }

    /// Whether this device has the control a binding key names.
    pub fn has(&self, key: BindingKey) -> bool {
        self.keys.contains(&key)
    }

    /// Resolves a canonical binding key against this device.
    ///
    /// A key that names no alignment means "the primary one". Most pads have a
    /// single D-pad and write `directional.n`; the N64's must say
    /// `directional.left.n`, because the C-buttons are a second directional and
    /// force both to declare where they sit. The unqualified key still has to
    /// reach it, so it falls back to the first cluster of that kind, in
    /// declaration order. Only that one answers to it — the C-buttons must be
    /// named.
    pub fn resolve(&self, key: BindingKey) -> Option<BindingKey> {
        if self.has(key) {
            return Some(key);
        }
        // An already aligned key asked for one particular cluster and must not
        // be given another, and `trigger.left` and `rumble.big` spend their only
        // component on the alignment, so neither has a cluster to fall back to.
        let (kind, class) = match key {
            BindingKey::Directional {
                alignment: None, ..
            } => ("directional", None),
            BindingKey::Analog {
                class,
                alignment: None,
                ..
            } => ("analog", Some(class)),
            // Both the absolute position and a relative axis: a device with
            // two mice has a primary one, and it is the same one for both.
            BindingKey::Pointer {
                alignment: None, ..
            } => ("pointer", None),
            _ => return None,
        };
        let cluster = self
            .clusters
            .iter()
            .find(|cluster| cluster.kind == kind && cluster.class == class)?;
        let candidate = key.with_alignment(cluster.alignment);
        self.has(candidate).then_some(candidate)
    }

    /// Whether the device declares an analog cluster of the given class —
    /// `gyroscope`, `accelerometer` — wherever on the device it sits.
    pub fn has_analog(&self, class: AnalogClass) -> bool {
        self.clusters
            .iter()
            .any(|cluster| cluster.kind == "analog" && cluster.class == Some(class))
    }

    pub fn same_topology(&self, other: &Self) -> bool {
        self.elements.len() == other.elements.len()
            && self
                .elements
                .iter()
                .zip(&other.elements)
                .all(|(left, right)| left.id == right.id && left.kind == right.kind)
    }

    /// Capabilities of this layout's SDL virtual gamepad.
    ///
    /// Which slot each cluster occupies is a mapping decision and lives in the
    /// shared projection table, not here — see [`crate::projection`]. A cluster
    /// the projection does not place has no SDL representation and is simply
    /// absent from the pad.
    pub fn sdl_output(&self, projection: &Projection) -> SdlOutputLayout {
        let mut output = SdlOutputLayout::default();
        // Every control is placed on its own. A device's *second* directional
        // goes direction by direction — the N64's C-buttons onto the right
        // stick — which needs no special case once the unit of mapping is a
        // control rather than a cluster.
        for element in &self.elements {
            match projection.target(self, element.key) {
                Some(Target::Key(target)) => output.claim(target),
                Some(Target::Both(first, second)) => {
                    output.claim(first);
                    output.claim(second);
                }
                // A control off the pad claims nothing on it.
                Some(Target::Transport(_) | Target::None) | None => {}
            }
        }
        // Pointer X/Y are Snowglass-specific raw joystick axes, outside the
        // gamepad mapping and present regardless of the selected topology.
        for (axis, _) in sdl::POINTER {
            output.axis_mask |= 1 << axis;
        }
        output
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SdlOutputLayout {
    pub button_mask: u32,
    pub axis_mask: u32,
    /// One bit per [`SdlSensor`], so a device with a left *and* a right
    /// accelerometer — a Wii Remote with a Nunchuk, or a pair of Joy-Cons —
    /// can declare both. `Copy` is worth keeping; a `Vec` here is not.
    pub sensor_mask: u8,
}

impl SdlOutputLayout {
    /// Claims whatever of the pad the given SDL control occupies.
    fn claim(&mut self, target: BindingKey) {
        match sdl::wire(target) {
            Some(Wire::Button(button)) => self.button_mask |= 1 << button.index(),
            Some(Wire::Axis(axis)) => self.axis_mask |= 1 << axis.index(),
            Some(Wire::RawAxis(index)) => self.axis_mask |= 1 << index,
            Some(Wire::Sensor(sensor)) => self.sensor_mask |= 1 << sensor as u8,
            // The projection rejects a target the pad does not have at load, so
            // this is unreachable for any table that parsed.
            None => {}
        }
    }

    pub fn sensors(&self) -> impl Iterator<Item = SdlSensor> + '_ {
        [
            SdlSensor::Accelerometer,
            SdlSensor::Gyroscope,
            SdlSensor::AccelerometerLeft,
            SdlSensor::GyroscopeLeft,
            SdlSensor::AccelerometerRight,
            SdlSensor::GyroscopeRight,
        ]
        .into_iter()
        .filter(move |sensor| self.sensor_mask & (1 << *sensor as u8) != 0)
    }
}

/// Every compiled consolespec input definition, materialized once.
///
/// There is no registry to hand around: consolespec is a compile-time table, so
/// the expansion of it is a property of the program rather than of any object.
/// Expansion is deterministic and every fallible invariant is validated by
/// `consolespec`'s build script.
fn layouts() -> &'static [InputLayout] {
    static LAYOUTS: OnceLock<Vec<InputLayout>> = OnceLock::new();
    LAYOUTS.get_or_init(|| {
        consolespec::InputSpec::all()
            .map(InputLayout::from_spec)
            .collect()
    })
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct RemapDatabase {
    #[serde(default)]
    pub profiles: Vec<RemapProfile>,
    /// Convenience bindings applied to every matching controller.
    #[serde(default)]
    pub bindings: HashMap<BindingKey, InputBinding>,
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
    pub bindings: HashMap<BindingKey, InputBinding>,
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

impl SdlButton {
    /// `SDL_GAMEPAD_BUTTON_*`'s value, which is also the wire position.
    pub const fn index(self) -> u8 {
        self as u8
    }

    /// The SDL constant this is, as emulator sources spell it in their own
    /// tables. Joining on this rather than on a position is what lets a
    /// generator survive SDL adding a control in the middle of the enum.
    pub const fn constant(self) -> &'static str {
        match self {
            Self::South => "SDL_GAMEPAD_BUTTON_SOUTH",
            Self::East => "SDL_GAMEPAD_BUTTON_EAST",
            Self::West => "SDL_GAMEPAD_BUTTON_WEST",
            Self::North => "SDL_GAMEPAD_BUTTON_NORTH",
            Self::Back => "SDL_GAMEPAD_BUTTON_BACK",
            Self::Guide => "SDL_GAMEPAD_BUTTON_GUIDE",
            Self::Start => "SDL_GAMEPAD_BUTTON_START",
            Self::LeftStick => "SDL_GAMEPAD_BUTTON_LEFT_STICK",
            Self::RightStick => "SDL_GAMEPAD_BUTTON_RIGHT_STICK",
            Self::LeftShoulder => "SDL_GAMEPAD_BUTTON_LEFT_SHOULDER",
            Self::RightShoulder => "SDL_GAMEPAD_BUTTON_RIGHT_SHOULDER",
            Self::DpadUp => "SDL_GAMEPAD_BUTTON_DPAD_UP",
            Self::DpadDown => "SDL_GAMEPAD_BUTTON_DPAD_DOWN",
            Self::DpadLeft => "SDL_GAMEPAD_BUTTON_DPAD_LEFT",
            Self::DpadRight => "SDL_GAMEPAD_BUTTON_DPAD_RIGHT",
            Self::Misc1 => "SDL_GAMEPAD_BUTTON_MISC1",
            Self::RightPaddle1 => "SDL_GAMEPAD_BUTTON_RIGHT_PADDLE1",
            Self::LeftPaddle1 => "SDL_GAMEPAD_BUTTON_LEFT_PADDLE1",
            Self::RightPaddle2 => "SDL_GAMEPAD_BUTTON_RIGHT_PADDLE2",
            Self::LeftPaddle2 => "SDL_GAMEPAD_BUTTON_LEFT_PADDLE2",
            Self::Touchpad => "SDL_GAMEPAD_BUTTON_TOUCHPAD",
        }
    }
}

impl SdlAxis {
    /// `SDL_GAMEPAD_AXIS_*`'s value, which is also the wire position.
    pub const fn index(self) -> u8 {
        self as u8
    }

    pub const fn constant(self) -> &'static str {
        match self {
            Self::LeftX => "SDL_GAMEPAD_AXIS_LEFTX",
            Self::LeftY => "SDL_GAMEPAD_AXIS_LEFTY",
            Self::RightX => "SDL_GAMEPAD_AXIS_RIGHTX",
            Self::RightY => "SDL_GAMEPAD_AXIS_RIGHTY",
            Self::LeftTrigger => "SDL_GAMEPAD_AXIS_LEFT_TRIGGER",
            Self::RightTrigger => "SDL_GAMEPAD_AXIS_RIGHT_TRIGGER",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct KeyboardMapping {
    /// Keyed canonically — `directional.n`, not `directional.left.n` — so one
    /// document serves every layout. A key the grammar does not admit fails the
    /// whole document rather than being dropped where nobody would notice.
    #[serde(default)]
    pub bindings: HashMap<BindingKey, String>,
}

impl KeyboardMapping {
    pub fn from_toml(toml: &str) -> Result<Self, String> {
        toml::from_str(toml).map_err(|error| format!("invalid keyboard mapping TOML: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use consolespec::key;

    #[test]
    fn parses_embedded_registry() {
        let xbox = InputLayout::get("XBOX_CONTROLLER").unwrap();
        assert!(xbox.has(key!(button.a)));
        assert_eq!(
            InputLayout::get("GAMECUBE_CONTROLLER").unwrap().name,
            "Nintendo GameCube Controller"
        );
        // A handheld's built-in controls are not a named product.
        let vita = InputLayout::get("PSV_LAYOUT").unwrap();
        assert_eq!(vita.name, "PSV_LAYOUT");
        assert!(xbox.has(key!(analog.stick.left.x+)));
        assert!(InputLayout::all().len() > 1);
    }

    #[test]
    fn every_compiled_spec_has_elements() {
        for spec in consolespec::InputSpec::all() {
            let layout = InputLayout::from_spec(spec);
            assert!(!layout.elements.is_empty(), "{}", spec.id());
        }
        assert_eq!(
            InputLayout::all().len(),
            consolespec::InputSpec::all().len()
        );
    }

    #[test]
    fn compiled_clusters_contain_elements() {
        let pad = InputLayout::get("GAMECUBE_CONTROLLER").unwrap();
        // `directions = 4` becomes four directionals, `axes = 2` two signed pairs.
        for expected in [
            key!(directional.n),
            key!(directional.e),
            key!(directional.s),
            key!(directional.w),
            key!(analog.stick.left.x+),
            key!(analog.stick.left.y-),
            key!(analog.stick.right.x+),
            key!(trigger.left),
            key!(trigger.right),
            key!(rumble.big),
            key!(rumble.small),
        ] {
            assert!(
                pad.has(expected),
                "{expected} missing from GAMECUBE_CONTROLLER"
            );
        }
        assert!(!pad.has(key!(button.clickl)));

        // A pointer contributes an absolute element and its relative axes.
        let remote = InputLayout::get("WII_REMOTE_CONTROLLER").unwrap();
        assert!(remote.has(key!(pointer)));
        assert!(remote.has(key!(pointer.x+)));
        assert!(remote.has(key!(analog.accelerometer.z-)));
        // `element = "0"` is the button printed "1" on the shell.
        assert!(remote.has(key!(button.0)));
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
        let pad = InputLayout::get("N64_CONTROLLER").unwrap();
        assert!(pad.has(key!(directional.left.n)));
        assert!(pad.has(key!(directional.right.n)));
        assert_eq!(
            pad.elements
                .iter()
                .filter(|element| element.id.starts_with("DirectionalN"))
                .count(),
            2
        );
        // An unaligned key still reaches the D-pad, never the C-buttons.
        assert_eq!(
            pad.resolve(key!(directional.n)),
            Some(key!(directional.left.n))
        );
    }

    #[test]
    fn parses_target_oriented_remap() {
        let remap = RemapDatabase::from_json(
            r#"{"bindings":{"button.a":{"button":"east"},"trigger.left":{"axis":"right_trigger","invert":false}}}"#,
        )
        .unwrap();
        assert!(matches!(
            remap.bindings[&key!(button.a)],
            InputBinding::Button {
                button: SdlButton::East
            }
        ));
    }

    #[test]
    fn compiles_sdl_capabilities_from_layout_elements() {
        let projection = crate::projection::Projection::embedded().unwrap();
        let switch = InputLayout::get("SWITCH_PRO_CONTROLLER")
            .unwrap()
            .sdl_output(&projection);
        assert_ne!(switch.button_mask & (1 << 0), 0); // A / south projection
        assert_ne!(switch.button_mask & (1 << 1), 0); // B / east projection
        assert_ne!(switch.axis_mask & (1 << 0), 0); // left X
        assert_ne!(switch.axis_mask & (1 << 5), 0); // right trigger
        let sensors: Vec<_> = switch.sensors().collect();
        assert!(sensors.contains(&SdlSensor::Accelerometer));
        assert!(sensors.contains(&SdlSensor::Gyroscope));
    }

    #[test]
    fn parses_embedded_keyboard_mapping() {
        let mapping = KeyboardMapping::from_toml(EMBEDDED_DEFAULT_KEYBOARD_MAPPING).unwrap();
        assert_eq!(
            mapping.bindings.get(&key!(button.a)).map(String::as_str),
            Some("KeyX")
        );
        assert_eq!(
            mapping
                .bindings
                .get(&key!(directional.n))
                .map(String::as_str),
            Some("ArrowUp")
        );
    }
}
