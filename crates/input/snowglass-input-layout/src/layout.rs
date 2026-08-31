use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use consolespec::input::{Alignment, AnalogClass, BindingKey, Component, Sign};

use crate::projection::{Projection, Target};
use crate::sdl::{self, SdlSensor, Wire};

pub const EMBEDDED_GAMECONTROLLER_DB: &str =
    include_str!("../SDL_GameControllerDB/gamecontrollerdb.txt");
pub const EMBEDDED_DEFAULT_KEYBOARD_MAPPING: &str = include_str!("../keyboard/DEFAULT.kb.toml");

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectionMode {
    Name,
    #[default]
    Position,
    Custom,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputElement {
    pub id: String,
    pub key: BindingKey,
    pub label: String,
    pub kind: String,
}

#[derive(Clone, Debug)]
pub struct Cluster {
    pub kind: &'static str,
    pub class: Option<AnalogClass>,
    pub alignment: Option<Alignment>,
    pub discriminator: Option<String>,
    pub arity: u8,
    pub elements: Vec<String>,
}

impl Cluster {
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
    pub name: String,
    pub elements: Vec<InputElement>,
    #[serde(skip)]
    pub clusters: Vec<Cluster>,
    #[serde(skip)]
    keys: HashSet<BindingKey>,
}

impl InputLayout {
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

    pub fn all() -> &'static [InputLayout] {
        layouts()
    }

    pub fn get(id: &str) -> Option<&'static InputLayout> {
        layouts().iter().find(|layout| layout.id == id)
    }

    pub fn has(&self, key: BindingKey) -> bool {
        self.keys.contains(&key)
    }

    pub fn resolve(&self, key: BindingKey) -> Option<BindingKey> {
        if self.has(key) {
            return Some(key);
        }
        let (kind, class) = match key {
            BindingKey::Directional {
                alignment: None, ..
            } => ("directional", None),
            BindingKey::Analog {
                class,
                alignment: None,
                ..
            } => ("analog", Some(class)),
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

    pub fn has_analog(&self, class: AnalogClass) -> bool {
        self.clusters
            .iter()
            .any(|cluster| cluster.kind == "analog" && cluster.class == Some(class))
    }

    pub fn sdl_wire(&self, key: BindingKey) -> Option<Wire> {
        self.sdl_key(key).and_then(sdl::wire)
    }

    pub fn sdl_key(&self, key: BindingKey) -> Option<BindingKey> {
        match Projection::shared().target(&self.id, key)? {
            Target::Key(target) => Some(target),
            Target::Both(..) | Target::Transport(_) | Target::None => None,
        }
    }

    pub fn accepts_axis_binding(&self, key: BindingKey) -> bool {
        self.sdl_key(key).is_some_and(|target| target.is_analog())
    }

    pub fn custom_binding(&self, target: BindingKey, source: BindingKey) -> Option<InputBinding> {
        match sdl::wire(source)? {
            Wire::Button(button) => Some(InputBinding::Button { button }),
            Wire::Axis(axis) => Some(InputBinding::Axis {
                axis,
                invert: self.sdl_key(target).is_some_and(Self::negative) != Self::negative(source),
            }),
            Wire::RawAxis(_) | Wire::Sensor(_) => None,
        }
    }

    pub fn custom_source(&self, target: BindingKey, binding: &InputBinding) -> Option<BindingKey> {
        let buttons = sdl::BUTTONS.iter().map(|(_, source)| *source);
        let axes = sdl::AXES.iter().flat_map(|(_, source)| {
            let opposite = match *source {
                BindingKey::Analog {
                    class,
                    alignment,
                    component,
                } => BindingKey::Analog {
                    class,
                    alignment,
                    component: Component::new(component.axis, Sign::Negative),
                },
                source => source,
            };
            [*source, opposite]
        });
        buttons
            .chain(axes)
            .find(|source| self.custom_binding(target, *source).as_ref() == Some(binding))
    }

    fn negative(key: BindingKey) -> bool {
        matches!(
            key,
            BindingKey::Analog {
                component: consolespec::input::Component {
                    sign: Sign::Negative,
                    ..
                },
                ..
            }
        )
    }

    pub fn custom_binding_targets(&self) -> impl Iterator<Item = BindingKey> + '_ {
        self.elements.iter().filter_map(|element| {
            matches!(
                self.sdl_wire(element.key),
                Some(Wire::Button(_) | Wire::Axis(_))
            )
            .then_some(element.key)
        })
    }

    pub fn default_gamepad_bindings(&self) -> impl Iterator<Item = (BindingKey, BindingKey)> + '_ {
        self.custom_binding_targets()
            .filter_map(|target| self.sdl_key(target).map(|source| (target, source)))
    }

    pub fn same_topology(&self, other: &Self) -> bool {
        self.elements.len() == other.elements.len()
            && self
                .elements
                .iter()
                .zip(&other.elements)
                .all(|(left, right)| left.id == right.id && left.kind == right.kind)
    }

    pub fn sdl_output(&self, projection: &Projection) -> SdlOutputLayout {
        let mut output = SdlOutputLayout::default();
        for element in &self.elements {
            match projection.target(&self.id, element.key) {
                Some(Target::Key(target)) => output.claim(target),
                Some(Target::Both(first, second)) => {
                    output.claim(first);
                    output.claim(second);
                }
                Some(Target::Transport(_) | Target::None) | None => {}
            }
        }
        for (axis, _) in sdl::POINTER {
            output.axis_mask |= 1 << axis;
        }
        // Reserve SDL's six extension buttons for action chords. They remain
        // unbound to game controls unless a layout explicitly projects one.
        output.button_mask |= 0b11_1111 << 15;
        output
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SdlOutputLayout {
    pub button_mask: u32,
    pub axis_mask: u32,
    pub sensor_mask: u8,
}

impl SdlOutputLayout {
    fn claim(&mut self, target: BindingKey) {
        match sdl::wire(target) {
            Some(Wire::Button(button)) => self.button_mask |= 1 << button.index(),
            Some(Wire::Axis(axis)) => self.axis_mask |= 1 << axis.index(),
            Some(Wire::RawAxis(index)) => self.axis_mask |= 1 << index,
            Some(Wire::Sensor(sensor)) => self.sensor_mask |= 1 << sensor as u8,
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
    pub const fn index(self) -> u8 {
        self as u8
    }

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
    use consolespec::input::key;

    #[test]
    fn parses_embedded_registry() {
        let xbox = InputLayout::get("XBOX_CONTROLLER").unwrap();
        assert!(xbox.has(key!(button.a)));
        assert_eq!(
            InputLayout::get("GAMECUBE_CONTROLLER").unwrap().name,
            "Nintendo GameCube Controller"
        );
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

        let remote = InputLayout::get("WII_REMOTE_CONTROLLER").unwrap();
        assert!(remote.has(key!(pointer)));
        assert!(remote.has(key!(pointer.x+)));
        assert!(remote.has(key!(analog.accelerometer.z-)));
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
        assert_ne!(switch.button_mask & (1 << 0), 0);
        assert_ne!(switch.button_mask & (1 << 1), 0);
        assert_ne!(switch.axis_mask & (1 << 0), 0);
        assert_ne!(switch.axis_mask & (1 << 5), 0);
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
