//! Input layouts and user-remapping documents.
//!
//! Layouts come from the `consolespec` crate, whose build script parses and
//! cross-checks the documents, so there is nothing to load and no registry to
//! own — [`InputLayout::all`] expands the compile-time table once, on first use.
//! An inputspec does not spell its elements out: it declares
//! *clusters* — `axes = 2, alignment = "left"` for a stick, `directions = 4`
//! D-pad — and the elements expand from them by the rules in [`expand`].
//!
//! Each element carries two names. The id (`ButtonA`) is a display handle and
//! depends on what else the device declares. The [`BindingKey`] (`button.a`) is
//! what a binding names: it is built from the cluster's own fields, so it is
//! stable against a device gaining a cluster, and it is the vocabulary the SDL
//! hooks, the hub, the keyboard defaults and the plugin configuration documents
//! share. See [`crate::key`] for the grammar.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use std::str::FromStr;

pub use crate::key::{
    Alignment, AnalogClass, Axis, BindingKey, ButtonElement, Component, Direction, Peripheral,
    RumbleMotor, RumbleSize, Sign,
};

use crate::projection::{Projection, SdlSensor, Slot};

pub const EMBEDDED_GAMECONTROLLER_DB: &str =
    include_str!("../SDL_GameControllerDB/gamecontrollerdb.txt");
pub const EMBEDDED_DEFAULT_KEYBOARD_MAPPING: &str = include_str!("../keyboard/DEFAULT.kb.toml");

/// A button's element name, capitalized into the element id it forms.
fn button_suffix(element: ButtonElement) -> String {
    match element {
        ButtonElement::Start => "Start".to_owned(),
        ButtonElement::Select => "Select".to_owned(),
        ButtonElement::Guide => "Guide".to_owned(),
        ButtonElement::ClickL => "ClickL".to_owned(),
        ButtonElement::ClickR => "ClickR".to_owned(),
        ButtonElement::Numbered(index) => index.to_string(),
        lettered => lettered.to_string().to_ascii_uppercase(),
    }
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
const HAT4: [Direction; 4] = [Direction::N, Direction::E, Direction::S, Direction::W];
const HAT8: [Direction; 8] = [
    Direction::N,
    Direction::E,
    Direction::S,
    Direction::W,
    Direction::Ne,
    Direction::Se,
    Direction::Sw,
    Direction::Nw,
];

const AXES: [Axis; 3] = [Axis::X, Axis::Y, Axis::Z];

/// `N` -> `"N"`, `Ne` -> `"NE"`, for the element id a direction forms.
fn direction_suffix(direction: Direction) -> String {
    direction.to_string().to_ascii_uppercase()
}

/// `X` -> `"X"`, for the element id an axis forms.
fn axis_suffix(axis: Axis) -> String {
    axis.to_string().to_ascii_uppercase()
}

/// The word an inputspec would have used for one signed half of an axis.
/// Sticks, sliders and pointers are named for the screen direction they push
/// toward; motion sensors for the rotation or acceleration they measure. `None`
/// is a pointer, which has a screen direction but no class of its own.
fn axis_label(class: Option<AnalogClass>, component: Component) -> &'static str {
    let positive = component.is_positive();
    match (class, component.axis, positive) {
        (Some(AnalogClass::Gyroscope), Axis::X, true) => "Roll Right",
        (Some(AnalogClass::Gyroscope), Axis::X, false) => "Roll Left",
        (Some(AnalogClass::Gyroscope), Axis::Y, true) => "Pitch Up",
        (Some(AnalogClass::Gyroscope), Axis::Y, false) => "Pitch Down",
        (Some(AnalogClass::Gyroscope), _, true) => "Yaw Right",
        (Some(AnalogClass::Gyroscope), _, false) => "Yaw Left",
        (Some(AnalogClass::Accelerometer), Axis::X, true) => "Right",
        (Some(AnalogClass::Accelerometer), Axis::X, false) => "Left",
        (Some(AnalogClass::Accelerometer), Axis::Y, true) => "Forward",
        (Some(AnalogClass::Accelerometer), Axis::Y, false) => "Backward",
        (Some(AnalogClass::Accelerometer), _, true) => "Up",
        (Some(AnalogClass::Accelerometer), _, false) => "Down",
        (_, Axis::X, true) => "Right",
        (_, Axis::X, false) => "Left",
        (_, Axis::Y, true) => "Up",
        (_, Axis::Y, false) => "Down",
        (_, _, true) => "Forward",
        (_, _, false) => "Backward",
    }
}

/// The two signed halves of one degree of freedom, positive first, as the
/// element ids and the binding keys both order them.
fn halves(axis: Axis) -> [(Component, &'static str); 2] {
    [
        (Component::new(axis, Sign::Positive), "Positive"),
        (Component::new(axis, Sign::Negative), "Negative"),
    ]
}

/// Reads an optional schema `alignment` field, rejecting a value the schema
/// does not allow rather than letting it through into a key nothing matches.
fn alignment_of(value: Option<consolespec::Alignment>) -> Option<Alignment> {
    value.map(|value| match value {
        consolespec::Alignment::Left => Alignment::Left,
        consolespec::Alignment::Right => Alignment::Right,
        consolespec::Alignment::Front => Alignment::Front,
        consolespec::Alignment::Rear => Alignment::Rear,
    })
}

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

/// Expands an inputspec's clusters into the element vocabulary.
///
/// Two clusters can expand onto the same id — the N64 pad declares two
/// directionals, the DualSense two small rumble motors — so a repeat takes an
/// ordinal suffix (`DirectionalN2`) in declaration order.
fn expand(spec: consolespec::InputSpec) -> Result<(Vec<InputElement>, Vec<Cluster>), String> {
    let mut elements: Vec<InputElement> = Vec::new();
    let mut clusters: Vec<Cluster> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut push = |id: String, key: BindingKey, label: String, kind: &str| -> String {
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
            id: id.clone(),
            key,
            label,
            kind: kind.to_owned(),
        });
        id
    };

    for button in spec.buttons() {
        let element = ButtonElement::from_str(button.element())
            .map_err(|_| format!("unknown button element `{}`", button.element()))?;
        let id = push(
            format!("Button{}", button_suffix(element)),
            BindingKey::Button(element),
            button.label().to_owned(),
            "Button",
        );
        clusters.push(Cluster {
            kind: "button",
            class: None,
            alignment: None,
            discriminator: Some(element.to_string()),
            arity: 1,
            elements: vec![id],
        });
    }
    for directional in spec.directionals() {
        let directions: &[Direction] = match directional.directions() {
            4 => &HAT4,
            8 => &HAT8,
            hats => return Err(format!("directional cluster with {hats} directions")),
        };
        let cluster = directional.label().unwrap_or("D-Pad");
        let alignment = alignment_of(directional.alignment());
        let ids = directions
            .iter()
            .map(|direction| {
                push(
                    format!("Directional{}", direction_suffix(*direction)),
                    BindingKey::Directional {
                        alignment,
                        direction: *direction,
                    },
                    format!("{cluster} {}", direction.label()),
                    "Directional",
                )
            })
            .collect();
        clusters.push(Cluster {
            kind: "directional",
            class: None,
            alignment,
            discriminator: alignment.map(|alignment| alignment.to_string()),
            arity: directional.directions(),
            elements: ids,
        });
    }
    for analog in spec.analog() {
        // `class` says what the cluster is, `alignment` says where it sits.
        // Both are optional: class defaults to a stick, and alignment appears
        // only where a device has two clusters of the same class to tell apart.
        let class = match analog.class() {
            consolespec::AnalogClass::Stick => AnalogClass::Stick,
            consolespec::AnalogClass::Slider => AnalogClass::Slider,
            consolespec::AnalogClass::Rotary => AnalogClass::Rotary,
            consolespec::AnalogClass::Gyroscope => AnalogClass::Gyroscope,
            consolespec::AnalogClass::Accelerometer => AnalogClass::Accelerometer,
        };
        let alignment = alignment_of(analog.alignment());
        let prefix = match (class, alignment) {
            // A device with one stick need not say which side it is on; the
            // sole stick is the primary one. A rotary dial has no side at all
            // and reads as the primary analog axis, as an SDL paddle does.
            (AnalogClass::Stick | AnalogClass::Rotary, Some(Alignment::Left) | None) => {
                "AxisLeftAnalog".to_owned()
            }
            (AnalogClass::Stick | AnalogClass::Rotary, Some(Alignment::Right)) => {
                "AxisRightAnalog".to_owned()
            }
            (AnalogClass::Gyroscope, None) => "GyroscopeAxis".to_owned(),
            (AnalogClass::Accelerometer, None) => "AccelerometerAxis".to_owned(),
            // A cluster sharing its class with another on the same device, or a
            // slider, has no name of its own in the *id* vocabulary — there is
            // no id for "the Nunchuk's accelerometer" or "the 3DS depth slider".
            // Qualifying by position or label keeps the id unique. Binding keys
            // have no such trouble; only the display handle does.
            (class, Some(side)) => format!(
                "{}{}Axis",
                pascal_case(side.as_str()),
                pascal_case(class.as_str())
            ),
            (class, None) => match analog.label() {
                Some(label) => format!("{}{}Axis", pascal_case(label), pascal_case(class.as_str())),
                None => return Err(format!("`{class}` cluster with no alignment or label")),
            },
        };
        if !(1..=3).contains(&analog.axes()) {
            return Err(format!("analog cluster with {} axes", analog.axes()));
        }
        let cluster =
            analog
                .label()
                .map(str::to_owned)
                .unwrap_or_else(|| match (class, alignment) {
                    (AnalogClass::Stick, None) => "Stick".to_owned(),
                    (AnalogClass::Stick, Some(Alignment::Left)) => "Left Stick".to_owned(),
                    (AnalogClass::Stick, Some(Alignment::Right)) => "Right Stick".to_owned(),
                    (AnalogClass::Rotary, _) => "Dial".to_owned(),
                    (class, _) => pascal_case(class.as_str()),
                });
        let mut ids = Vec::new();
        for axis in &AXES[..usize::from(analog.axes())] {
            for (component, sign) in halves(*axis) {
                ids.push(push(
                    format!("{prefix}{sign}{}", axis_suffix(*axis)),
                    BindingKey::Analog {
                        class,
                        alignment,
                        component,
                    },
                    format!("{cluster} {}", axis_label(Some(class), component)),
                    if component.is_positive() {
                        "AxisPositive"
                    } else {
                        "AxisNegative"
                    },
                ));
            }
        }
        clusters.push(Cluster {
            kind: "analog",
            class: Some(class),
            alignment,
            discriminator: alignment.map(|alignment| alignment.to_string()),
            arity: analog.axes(),
            elements: ids,
        });
    }
    for trigger in spec.triggers() {
        let alignment = match alignment_of(Some(trigger.alignment())) {
            Some(side @ (Alignment::Left | Alignment::Right)) => side,
            _ => return Err("trigger alignment is not left or right".to_owned()),
        };
        let side = pascal_case(alignment.as_str());
        let id = push(
            format!("Trigger{side}"),
            BindingKey::Trigger { alignment },
            trigger
                .label()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{side} Trigger")),
            "Trigger",
        );
        clusters.push(Cluster {
            kind: "trigger",
            class: None,
            alignment: Some(alignment),
            discriminator: Some(alignment.to_string()),
            arity: 1,
            elements: vec![id],
        });
    }
    for rumble in spec.rumble() {
        let size = match rumble.size() {
            consolespec::RumbleSize::Big => RumbleSize::Big,
            consolespec::RumbleSize::Small => RumbleSize::Small,
        };
        let (suffix, fallback) = match size {
            RumbleSize::Big => ("Big", "Strong Rumble"),
            RumbleSize::Small => ("Small", "Weak Rumble"),
        };
        // A device with two motors of one size tells them apart by side.
        let motor = match alignment_of(rumble.alignment()) {
            Some(alignment) => RumbleMotor::Aligned(alignment),
            None => RumbleMotor::Size(size),
        };
        let id = push(
            format!("Rumble{suffix}"),
            BindingKey::Rumble(motor),
            rumble
                .label()
                .map(str::to_owned)
                .unwrap_or_else(|| fallback.to_owned()),
            "Rumble",
        );
        clusters.push(Cluster {
            kind: "rumble",
            class: None,
            alignment: None,
            discriminator: Some(motor.to_string()),
            arity: 1,
            elements: vec![id],
        });
    }
    for pointer in spec.pointers() {
        if !(2..=3).contains(&pointer.dimensions()) {
            return Err(format!("pointer with {} dimensions", pointer.dimensions()));
        }
        let cluster = pointer.label().unwrap_or("Pointer");
        let alignment = alignment_of(pointer.alignment());
        // The absolute position, then the relative axis each dimension carries.
        push(
            format!("Pointer{}D", pointer.dimensions()),
            BindingKey::Pointer {
                alignment,
                component: None,
            },
            cluster.to_owned(),
            "Pointer",
        );
        let mut ids = Vec::new();
        for axis in &AXES[..usize::from(pointer.dimensions())] {
            for (component, sign) in halves(*axis) {
                ids.push(push(
                    format!("PointerAxis{sign}{}", axis_suffix(*axis)),
                    BindingKey::Pointer {
                        alignment,
                        component: Some(component),
                    },
                    format!("{cluster} {}", axis_label(None, component)),
                    if component.is_positive() {
                        "AxisPositive"
                    } else {
                        "AxisNegative"
                    },
                ));
            }
        }
        clusters.push(Cluster {
            kind: "pointer",
            class: None,
            alignment,
            discriminator: alignment.map(|alignment| alignment.to_string()),
            arity: pointer.dimensions(),
            elements: ids,
        });
    }
    let peripherals = spec
        .touchscreens()
        .map(|entry| {
            (
                entry.label(),
                entry.alignment(),
                "Touchscreen",
                Peripheral::Touchscreen,
            )
        })
        .chain(spec.microphones().map(|entry| {
            (
                entry.label(),
                entry.alignment(),
                "Microphone",
                Peripheral::Microphone,
            )
        }))
        .chain(spec.cameras().map(|entry| {
            (
                entry.label(),
                entry.alignment(),
                "Camera",
                Peripheral::Camera,
            )
        }));
    for (label, source_alignment, name, kind) in peripherals {
        let alignment = alignment_of(source_alignment);
        let id = push(
            name.to_owned(),
            BindingKey::Peripheral { kind, alignment },
            label.map(str::to_owned).unwrap_or_else(|| name.to_owned()),
            name,
        );
        clusters.push(Cluster {
            kind: kind.as_str(),
            class: None,
            alignment,
            discriminator: alignment.map(|alignment| alignment.to_string()),
            arity: 1,
            elements: vec![id],
        });
    }

    if elements.is_empty() {
        return Err("no elements".to_owned());
    }
    elements.sort_by(|left, right| left.id.cmp(&right.id));
    Ok((elements, clusters))
}

impl InputLayout {
    /// Expands one compile-time validated consolespec input definition.
    pub fn from_spec(spec: consolespec::InputSpec) -> Result<Self, String> {
        let (elements, clusters) =
            expand(spec).map_err(|error| format!("inputspec {}: {error}", spec.id()))?;
        let keys = elements.iter().map(|element| element.key).collect();
        let name = spec.name().unwrap_or_else(|| spec.id()).to_owned();
        Ok(Self {
            id: spec.id().to_owned(),
            name,
            elements,
            clusters,
            keys,
        })
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
        // Only a key with a component after the alignment has a cluster to fall
        // back to: `trigger.left` and `rumble.big` end at theirs, and an already
        // aligned key asked for one particular cluster and must not get another.
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
                alignment: None,
                component: Some(_),
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
        for cluster in &self.clusters {
            // Components are checked first, and win. A device's *second*
            // directional is placed direction by direction — the N64's
            // C-buttons onto the right stick — and must not fall back to the
            // hat, which its first directional already holds.
            if let Some(slots) = projection.component_slots(&self.id, cluster) {
                for slot in slots {
                    output.claim(slot);
                }
                continue;
            }
            if let Some(slot) = projection.resolve(&self.id, cluster) {
                output.claim(slot);
            }
        }
        // Pointer X/Y are Snowglass-specific raw joystick axes, outside the
        // gamepad mapping and present regardless of the selected topology.
        output.axis_mask |= (1 << 6) | (1 << 7);
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
    fn claim(&mut self, slot: Slot) {
        match slot {
            Slot::Button(index) => self.button_mask |= 1 << index,
            Slot::Hat => self.button_mask |= 0b1111 << 11,
            Slot::Stick(axis) => self.axis_mask |= 0b11 << axis,
            Slot::Axis(axis) | Slot::HalfAxis { axis, .. } => self.axis_mask |= 1 << axis,
            Slot::Pointer => self.axis_mask |= (1 << 6) | (1 << 7),
            Slot::Sensor(sensor) => self.sensor_mask |= 1 << sensor as u8,
            Slot::Keyboard => {}
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

/// Every consolespec input definition, expanded once.
///
/// There is no registry to hand around: consolespec is a compile-time table, so
/// the expansion of it is a property of the program rather than of any object.
/// Expansion is deterministic and its inputs were validated when the crate was
/// built, so a failure here is a build bug — `every_spec_expands` is what keeps
/// it from being discovered at runtime.
fn layouts() -> &'static [InputLayout] {
    static LAYOUTS: OnceLock<Vec<InputLayout>> = OnceLock::new();
    LAYOUTS.get_or_init(|| {
        consolespec::InputSpec::all()
            .map(|spec| {
                InputLayout::from_spec(spec)
                    .unwrap_or_else(|error| panic!("consolespec inputspec is unusable: {error}"))
            })
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

    fn key(text: &str) -> BindingKey {
        text.parse().expect("test binding key")
    }

    #[test]
    fn parses_embedded_registry() {
        let xbox = InputLayout::get("XBOX_CONTROLLER").unwrap();
        assert!(xbox.has(key("button.a")));
        assert_eq!(
            InputLayout::get("GAMECUBE_CONTROLLER").unwrap().name,
            "Nintendo GameCube Controller"
        );
        // A handheld's built-in controls are not a named product.
        let vita = InputLayout::get("PSV_LAYOUT").unwrap();
        assert_eq!(vita.name, "PSV_LAYOUT");
        assert!(xbox.has(key("analog.stick.left.x+")));
        assert!(InputLayout::all().iter().count() > 1);
    }

    /// `layouts()` panics rather than returning an error, because a consolespec
    /// document that will not expand is a build bug and not a runtime condition.
    /// That is only defensible if every document does expand, which is this.
    #[test]
    fn every_spec_expands() {
        for spec in consolespec::InputSpec::all() {
            InputLayout::from_spec(spec).unwrap_or_else(|error| panic!("{error}"));
        }
        assert_eq!(InputLayout::all().len(), consolespec::InputSpec::all().len());
    }

    #[test]
    fn expands_clusters_into_elements() {
        let pad = InputLayout::get("GAMECUBE_CONTROLLER").unwrap();
        // `directions = 4` becomes four directionals, `axes = 2` two signed pairs.
        for text in [
            "directional.n",
            "directional.e",
            "directional.s",
            "directional.w",
            "analog.stick.left.x+",
            "analog.stick.left.y-",
            "analog.stick.right.x+",
            "trigger.left",
            "trigger.right",
            "rumble.big",
            "rumble.small",
        ] {
            assert!(
                pad.has(key(text)),
                "{text} missing from GAMECUBE_CONTROLLER"
            );
        }
        assert!(!pad.has(key("button.clickl")));

        // A pointer contributes an absolute element and its relative axes.
        let remote = InputLayout::get("WII_REMOTE_CONTROLLER").unwrap();
        assert!(remote.has(key("pointer")));
        assert!(remote.has(key("pointer.x+")));
        assert!(remote.has(key("analog.accelerometer.z-")));
        // `element = "0"` is the button printed "1" on the shell.
        assert!(remote.has(key("button.0")));
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
        assert!(pad.has(key("directional.left.n")));
        assert!(pad.has(key("directional.right.n")));
        assert_eq!(
            pad.elements
                .iter()
                .filter(|element| element.id.starts_with("DirectionalN"))
                .count(),
            2
        );
        // An unaligned key still reaches the D-pad, never the C-buttons.
        assert_eq!(
            pad.resolve(key("directional.n")),
            Some(key("directional.left.n"))
        );
    }

    #[test]
    fn parses_target_oriented_remap() {
        let remap = RemapDatabase::from_json(
            r#"{"bindings":{"button.a":{"button":"east"},"trigger.left":{"axis":"right_trigger","invert":false}}}"#,
        )
        .unwrap();
        assert!(matches!(
            remap.bindings[&key("button.a")],
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
            mapping.bindings.get(&key("button.a")).map(String::as_str),
            Some("KeyX")
        );
        assert_eq!(
            mapping
                .bindings
                .get(&key("directional.n"))
                .map(String::as_str),
            Some("ArrowUp")
        );
    }
}
