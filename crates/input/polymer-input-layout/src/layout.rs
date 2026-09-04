use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use consolespec::input::{Alignment, AnalogClass, BindingKey, Component, Sign};
use polymer_config::input::ControllerBindings;

use crate::model::{self, Control, Sensor};
use crate::projection::{Projection, Target};

pub const EMBEDDED_DEFAULT_KEYBOARD_MAPPING: &str = include_str!("../keyboard/DEFAULT.kb.toml");

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

    pub fn projected_control(&self, key: BindingKey) -> Option<Control> {
        self.projected_key(key).and_then(Control::from_key)
    }

    pub fn projected_key(&self, key: BindingKey) -> Option<BindingKey> {
        match Projection::shared().target(&self.id, key)? {
            Target::Key(target) => Some(target),
            Target::Both(..) | Target::Transport(_) | Target::None => None,
        }
    }

    pub fn accepts_axis_binding(&self, key: BindingKey) -> bool {
        self.projected_key(key)
            .is_some_and(|target| target.is_analog())
    }

    pub fn custom_binding(&self, target: BindingKey, source: BindingKey) -> Option<InputBinding> {
        match Control::from_key(source)? {
            Control::Button(index) => Some(InputBinding::Button {
                button: model::StandardGamepad::BUTTONS[usize::from(index)],
            }),
            Control::Axis(index) if usize::from(index) < model::StandardGamepad::AXES.len() => {
                Some(InputBinding::Axis {
                    axis: model::StandardGamepad::AXES[usize::from(index)],
                    invert: self.projected_key(target).is_some_and(Self::negative)
                        != Self::negative(source),
                })
            }
            Control::Axis(_) | Control::Sensor(_) => None,
        }
    }

    pub fn default_controller_bindings(&self) -> ControllerBindings {
        ControllerBindings::new(
            self.default_gamepad_bindings()
                .map(|(target, source)| (target, Some(source))),
        )
    }

    pub fn validate_controller_bindings(
        &self,
        bindings: &ControllerBindings,
    ) -> Result<(), String> {
        for (target, source) in bindings.iter() {
            if !self.has(*target) {
                return Err(format!("input layout {} has no element {target}", self.id));
            }
            if let Some(source) = source {
                let binding = self.custom_binding(*target, *source).ok_or_else(|| {
                    format!("input source '{source}' is not a physical gamepad expression")
                })?;
                if matches!(binding, InputBinding::Axis { .. })
                    && !self.accepts_axis_binding(*target)
                {
                    return Err(format!("element {target} does not accept an axis binding"));
                }
            }
        }
        Ok(())
    }

    pub fn custom_source(&self, target: BindingKey, binding: &InputBinding) -> Option<BindingKey> {
        let buttons = model::StandardGamepad::BUTTONS.iter().copied();
        let axes = model::StandardGamepad::AXES.iter().flat_map(|source| {
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
            self.projected_control(element.key)
                .is_some_and(|control| match control {
                    Control::Button(_) => true,
                    Control::Axis(index) => usize::from(index) < model::StandardGamepad::AXES.len(),
                    Control::Sensor(_) => false,
                })
                .then_some(element.key)
        })
    }

    pub fn default_gamepad_bindings(&self) -> impl Iterator<Item = (BindingKey, BindingKey)> + '_ {
        self.custom_binding_targets()
            .filter_map(|target| self.projected_key(target).map(|source| (target, source)))
    }

    pub fn same_topology(&self, other: &Self) -> bool {
        self.elements.len() == other.elements.len()
            && self
                .elements
                .iter()
                .zip(&other.elements)
                .all(|(left, right)| left.id == right.id && left.kind == right.kind)
    }

    pub fn output_layout(&self, projection: &Projection) -> OutputLayout {
        let mut output = OutputLayout::default();
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
        for index in 0..model::StandardGamepad::POINTER_AXES.len() {
            output.axis_mask |= 1 << (model::StandardGamepad::AXES.len() + index);
        }
        // Reserve the six extension buttons for action chords. They remain
        // unbound to game controls unless a layout explicitly projects one.
        output.button_mask |= 0b11_1111 << 15;
        output
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OutputLayout {
    pub button_mask: u32,
    pub axis_mask: u32,
    pub sensor_mask: u8,
}

impl OutputLayout {
    fn claim(&mut self, target: BindingKey) {
        match Control::from_key(target) {
            Some(Control::Button(index)) => self.button_mask |= 1 << index,
            Some(Control::Axis(index)) => self.axis_mask |= 1 << index,
            Some(Control::Sensor(sensor)) => {
                if let Some(index) = sensor.index() {
                    self.sensor_mask |= 1 << index;
                }
            }
            None => {}
        }
    }

    pub fn sensors(&self) -> impl Iterator<Item = Sensor> + '_ {
        Sensor::ALL.into_iter().filter(move |sensor| {
            sensor
                .index()
                .is_some_and(|index| self.sensor_mask & (1 << index) != 0)
        })
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
        button: BindingKey,
    },
    Axis {
        axis: BindingKey,
        #[serde(default)]
        invert: bool,
    },
    Disabled {
        disabled: bool,
    },
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
            r#"{"bindings":{"button.a":{"button":"button.b"},"trigger.left":{"axis":"trigger.right","invert":false}}}"#,
        )
        .unwrap();
        assert!(matches!(
            remap.bindings[&key!(button.a)],
            InputBinding::Button {
                button: key!(button.b)
            }
        ));
    }

    #[test]
    fn compiles_output_capabilities_from_layout_elements() {
        let projection = crate::projection::Projection::embedded().unwrap();
        let switch = InputLayout::get("SWITCH_PRO_CONTROLLER")
            .unwrap()
            .output_layout(&projection);
        assert_ne!(switch.button_mask & (1 << 0), 0);
        assert_ne!(switch.button_mask & (1 << 1), 0);
        assert_ne!(switch.axis_mask & (1 << 0), 0);
        assert_ne!(switch.axis_mask & (1 << 5), 0);
        let sensors: Vec<_> = switch.sensors().collect();
        assert!(sensors.contains(&Sensor::new(AnalogClass::Accelerometer, None)));
        assert!(sensors.contains(&Sensor::new(AnalogClass::Gyroscope, None)));
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
