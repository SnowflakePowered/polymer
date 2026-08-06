//! Projection of consolespec clusters onto SDL's virtual gamepad.
//!
//! This is mapping, not description. An inputspec says a cluster has two
//! degrees of freedom; `projection/SDL.proj.toml` decides which SDL axes carry
//! them. Keeping it in one shared table means "which physical button is C-Up"
//! is the same in every emulator, and that the six-slot overflow budget can be
//! checked globally — a per-plugin table could not tell you that two plugins
//! had allocated the same slot differently.
//!
//! The allocation follows SDL's own `gamecontrollerdb`: unused standard slots
//! are filled before any spare. A Wii Remote's 1 and 2 become X and Y because a
//! Wii Remote has no X/Y face buttons; a Saturn pad's C and Z become shoulders
//! because its shoulder slots are free.

use serde::Deserialize;
use std::collections::HashMap;

use crate::layout::{Cluster, InputLayout};

pub const EMBEDDED_SDL_PROJECTION: &str = include_str!("../projection/SDL.proj.toml");

/// A slot on the virtual gamepad, or another transport entirely.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Slot {
    /// A gamepad button, by semantic index (see `SdlButton`'s declaration order).
    Button(u8),
    /// The four hat buttons, as one claim.
    Hat,
    /// A whole stick: two axes, the first being X.
    Stick(u8),
    /// One axis. Triggers and single-degree controls land here.
    Axis(u8),
    /// One signed half of an axis, for a digital control driving a stick.
    HalfAxis {
        axis: u8,
        positive: bool,
    },
    /// Absolute pointer, on the two raw axes outside the gamepad mapping.
    Pointer,
    Sensor(SdlSensor),
    /// Not on the gamepad at all. The keypad of a 5200 or Intellivision is a
    /// keypad; the emulator reads it through the keyboard transport.
    Keyboard,
}

/// SDL models a device with two of one sensor — the Joy-Cons, or a Wii Remote
/// with a Nunchuk — as a left and a right instance. The numeric `SDL_SensorType`
/// values live in the hook, which owns SDL's constants.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SdlSensor {
    Accelerometer,
    Gyroscope,
    AccelerometerLeft,
    GyroscopeLeft,
    AccelerometerRight,
    GyroscopeRight,
}

fn slot(name: &str) -> Option<Slot> {
    // Semantic button indices, in `SdlButton`'s declaration order.
    let button = |index| Some(Slot::Button(index));
    Some(match name {
        "south" => return button(0),
        "east" => return button(1),
        "west" => return button(2),
        "north" => return button(3),
        "back" => return button(4),
        "guide" => return button(5),
        "start" => return button(6),
        "left-stick-click" => return button(7),
        "right-stick-click" => return button(8),
        "left-shoulder" => return button(9),
        "right-shoulder" => return button(10),
        "misc1" => return button(15),
        "right-paddle1" => return button(16),
        "left-paddle1" => return button(17),
        "right-paddle2" => return button(18),
        "left-paddle2" => return button(19),
        "touchpad" => return button(20),
        "hat" => Slot::Hat,
        "left-stick" => Slot::Stick(0),
        "right-stick" => Slot::Stick(2),
        "left-stick.x" => Slot::Axis(0),
        "left-stick.y" => Slot::Axis(1),
        "right-stick.x" => Slot::Axis(2),
        "right-stick.y" => Slot::Axis(3),
        "left-trigger" => Slot::Axis(4),
        "right-trigger" => Slot::Axis(5),
        "left-stick.x-" => Slot::HalfAxis {
            axis: 0,
            positive: false,
        },
        "left-stick.x+" => Slot::HalfAxis {
            axis: 0,
            positive: true,
        },
        "left-stick.y-" => Slot::HalfAxis {
            axis: 1,
            positive: false,
        },
        "left-stick.y+" => Slot::HalfAxis {
            axis: 1,
            positive: true,
        },
        "right-stick.x-" => Slot::HalfAxis {
            axis: 2,
            positive: false,
        },
        "right-stick.x+" => Slot::HalfAxis {
            axis: 2,
            positive: true,
        },
        "right-stick.y-" => Slot::HalfAxis {
            axis: 3,
            positive: false,
        },
        "right-stick.y+" => Slot::HalfAxis {
            axis: 3,
            positive: true,
        },
        "pointer" => Slot::Pointer,
        "gyroscope" => Slot::Sensor(SdlSensor::Gyroscope),
        "gyroscope-left" => Slot::Sensor(SdlSensor::GyroscopeLeft),
        "gyroscope-right" => Slot::Sensor(SdlSensor::GyroscopeRight),
        "accelerometer" => Slot::Sensor(SdlSensor::Accelerometer),
        "accelerometer-left" => Slot::Sensor(SdlSensor::AccelerometerLeft),
        "accelerometer-right" => Slot::Sensor(SdlSensor::AccelerometerRight),
        "keyboard" => Slot::Keyboard,
        _ => return None,
    })
}

#[derive(Debug, Deserialize)]
struct Document {
    #[serde(default)]
    defaults: HashMap<String, toml::Value>,
    #[serde(default)]
    layouts: HashMap<String, HashMap<String, String>>,
}

#[derive(Clone, Debug)]
pub struct Projection {
    /// Cluster path -> slot name, for every layout that does not override it.
    defaults: HashMap<String, String>,
    /// Layout id -> cluster path -> slot name.
    layouts: HashMap<String, HashMap<String, String>>,
}

/// Flattens `[defaults.analog] stick.left = "…"` into `analog.stick.left`.
fn flatten(prefix: &str, value: &toml::Value, into: &mut HashMap<String, String>) {
    match value {
        toml::Value::String(slot) => {
            into.insert(prefix.trim_end_matches('.').to_owned(), slot.clone());
        }
        toml::Value::Table(table) => {
            for (key, nested) in table {
                let path = if key.is_empty() {
                    prefix.trim_end_matches('.').to_owned()
                } else {
                    format!("{prefix}{key}")
                };
                flatten(&format!("{path}."), nested, into);
            }
        }
        _ => {}
    }
}

impl Projection {
    pub fn embedded() -> Result<Self, String> {
        Self::from_toml(EMBEDDED_SDL_PROJECTION)
    }

    pub fn from_toml(source: &str) -> Result<Self, String> {
        let document: Document =
            toml::from_str(source).map_err(|error| format!("invalid projection: {error}"))?;
        let mut defaults = HashMap::new();
        for (kind, value) in &document.defaults {
            flatten(&format!("{kind}."), value, &mut defaults);
        }
        for (path, name) in &defaults {
            if slot(name).is_none() {
                return Err(format!(
                    "projection default {path} names unknown slot {name}"
                ));
            }
        }
        for (layout, table) in &document.layouts {
            for (path, name) in table {
                if slot(name).is_none() {
                    return Err(format!(
                        "projection {layout}.{path} names unknown slot {name}"
                    ));
                }
            }
        }
        Ok(Self {
            defaults,
            layouts: document.layouts,
        })
    }

    /// Where one cluster of `layout` lands, if anywhere.
    ///
    /// A layout entry wins over the defaults; a cluster the defaults do not
    /// cover and the layout does not place has no SDL representation.
    pub fn resolve(&self, layout: &str, cluster: &Cluster) -> Option<Slot> {
        let path = cluster.path();
        let overrides = self.layouts.get(layout);
        // A directional projected per-component names each one; the whole
        // cluster is looked up first so `hat` stays a single claim.
        overrides
            .and_then(|table| table.get(&path))
            .or_else(|| self.defaults.get(&path))
            .or_else(|| {
                // `analog.stick.left` falls back to `analog.stick` and then
                // `analog`, so a default need not enumerate every alignment.
                let mut probe = path.as_str();
                loop {
                    probe = probe.rsplit_once('.')?.0;
                    if let Some(name) = overrides
                        .and_then(|table| table.get(probe))
                        .or_else(|| self.defaults.get(probe))
                    {
                        return Some(name);
                    }
                }
            })
            .and_then(|name| slot(name))
    }

    /// Where one component of a cluster lands — used where a directional is
    /// projected onto individual slots rather than the hat.
    pub fn resolve_component(
        &self,
        layout: &str,
        cluster: &Cluster,
        component: &str,
    ) -> Option<Slot> {
        let path = format!("{}.{component}", cluster.path());
        self.layouts
            .get(layout)
            .and_then(|table| table.get(&path))
            .and_then(|name| slot(name))
    }

    /// Every component slot of `cluster`, when the layout places all of them
    /// individually. `None` when it places none or only some, so a partial
    /// entry is a gap rather than a silent half-projection.
    pub fn component_slots(&self, layout: &str, cluster: &Cluster) -> Option<Vec<Slot>> {
        let components = directions(cluster);
        if components.is_empty() {
            return None;
        }
        let slots: Vec<_> = components
            .iter()
            .filter_map(|component| self.resolve_component(layout, cluster, component))
            .collect();
        (slots.len() == components.len()).then_some(slots)
    }

    pub fn layout_names(&self) -> impl Iterator<Item = &str> {
        self.layouts.keys().map(String::as_str)
    }

    /// Every cluster of `layout` that reaches neither the gamepad nor another
    /// declared transport. A binding for one of these could never fire.
    pub fn unprojected<'a>(&self, layout: &'a InputLayout) -> Vec<&'a Cluster> {
        layout
            .clusters
            .iter()
            .filter(|cluster| {
                self.component_slots(&layout.id, cluster).is_none()
                    && self.resolve(&layout.id, cluster).is_none()
            })
            .collect()
    }
}

/// The component names of a directional cluster, empty for anything else.
pub(crate) fn directions(cluster: &Cluster) -> &'static [&'static str] {
    match (cluster.kind, cluster.arity) {
        ("directional", 4) => &["n", "e", "s", "w"],
        ("directional", 8) => &["n", "e", "s", "w", "ne", "se", "sw", "nw"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::LayoutRegistry;

    #[test]
    fn embedded_projection_parses() {
        let projection = Projection::embedded().unwrap();
        assert!(projection.layout_names().count() > 20);
    }

    #[test]
    fn every_layout_is_fully_projected() {
        let registry = LayoutRegistry::embedded().unwrap();
        let projection = Projection::embedded().unwrap();
        let mut gaps = Vec::new();
        for layout in registry.layouts() {
            for cluster in projection.unprojected(layout) {
                // Output and non-gamepad clusters have no SDL input slot by
                // design; everything a binding can name must reach somewhere.
                if matches!(
                    cluster.kind,
                    "rumble" | "touchscreen" | "microphone" | "camera"
                ) {
                    continue;
                }
                gaps.push(format!("{}: {}", layout.id, cluster.path()));
            }
        }
        assert!(gaps.is_empty(), "clusters with no projection: {gaps:#?}");
    }

    #[test]
    fn saturn_pads_differ_by_what_else_they_carry() {
        let registry = LayoutRegistry::embedded().unwrap();
        let projection = Projection::embedded().unwrap();
        let find = |id: &str, path: &str| {
            let layout = registry.layout(id).unwrap();
            let cluster = layout
                .clusters
                .iter()
                .find(|cluster| cluster.path() == path)
                .unwrap();
            projection.resolve(id, cluster).unwrap()
        };
        // The standard pad's L/R are digital shoulders, so C and Z spill to the
        // triggers. The 3D pad's L/R are analog triggers, so C and Z take the
        // shoulders instead. Same buttons, opposite slots, no hand-written rule.
        assert_eq!(find("SAT_CONTROLLER", "button.c"), Slot::Axis(4));
        assert_eq!(find("SAT_CONTROLLER", "button.z"), Slot::Axis(5));
        assert_eq!(find("SAT_3D_CONTROLLER", "button.c"), Slot::Button(9));
        assert_eq!(find("SAT_3D_CONTROLLER", "button.z"), Slot::Button(10));
    }

    #[test]
    fn a_nunchuk_carries_two_accelerometers() {
        let registry = LayoutRegistry::embedded().unwrap();
        let projection = Projection::embedded().unwrap();
        let layout = registry.layout("WII_REMOTE_NUNCHUCK_CONTROLLER").unwrap();
        let sensors: Vec<_> = layout
            .clusters
            .iter()
            .filter_map(|cluster| match projection.resolve(&layout.id, cluster) {
                Some(Slot::Sensor(sensor)) => Some(sensor),
                _ => None,
            })
            .collect();
        assert!(sensors.contains(&SdlSensor::AccelerometerRight));
        assert!(sensors.contains(&SdlSensor::AccelerometerLeft));
        assert!(sensors.contains(&SdlSensor::Gyroscope));
    }
}

#[cfg(test)]
mod smoke {
    use super::*;
    use crate::layout::LayoutRegistry;

    /// The pad each layout actually presents. These were read off the
    /// projection and checked by hand against the physical devices; they exist
    /// to catch a change in the allocation rule, which has silently mis-placed
    /// clusters twice — two accelerometers colliding on one sensor slot, and a
    /// second directional inheriting the hat instead of the right stick.
    #[test]
    fn representative_layouts_project_as_expected() {
        let registry = LayoutRegistry::embedded().unwrap();
        let projection = Projection::embedded().unwrap();
        let pad = |id: &str| {
            let out = registry.layout(id).unwrap().sdl_output(&projection);
            (
                out.button_mask,
                out.axis_mask,
                out.sensors().collect::<Vec<_>>(),
            )
        };
        const HAT: u32 = 0b1111 << 11;
        const FACE: u32 = 0b1111;
        const POINTER: u32 = 0b11 << 6;

        // A full modern pad: every standard slot, every axis.
        let (buttons, axes, sensors) = pad("XBOX_CONTROLLER");
        assert_eq!(buttons, 0b0111_1111_1111_1111);
        assert_eq!(axes, 0b1111_1111);
        assert!(sensors.is_empty());

        // No left shoulder — Z takes the right one, and the digital L/R clicks
        // share the trigger axes with the analog travel.
        let (buttons, axes, _) = pad("GAMECUBE_CONTROLLER");
        assert_eq!(buttons & (1 << 9), 0, "GameCube has no left shoulder");
        assert_ne!(buttons & (1 << 10), 0, "Z is the right shoulder");
        assert_eq!(buttons & HAT, HAT);
        assert_eq!(axes, 0b1111_1111);

        // C-buttons drive the right stick, Z the right trigger — 1:1 with the
        // NSO N64 controller's own gamecontrollerdb entry.
        let (buttons, axes, _) = pad("N64_CONTROLLER");
        assert_eq!(axes & 0b1100, 0b1100, "C-buttons occupy the right stick");
        assert_ne!(axes & (1 << 5), 0, "Z is the right trigger");
        assert_eq!(axes & (1 << 4), 0, "an N64 pad has no left trigger");
        assert_ne!(buttons & (1 << 9), 0);
        assert_ne!(buttons & (1 << 10), 0);

        // Two accelerometers: the Remote's and the Nunchuk's.
        let (_, axes, sensors) = pad("WII_REMOTE_NUNCHUCK_CONTROLLER");
        assert_eq!(sensors.len(), 3);
        assert!(sensors.contains(&SdlSensor::AccelerometerLeft));
        assert!(sensors.contains(&SdlSensor::AccelerometerRight));
        assert!(sensors.contains(&SdlSensor::Gyroscope));
        assert_eq!(axes & POINTER, POINTER, "the IR pointer is present");

        // The keypad routes to the keyboard, so it claims no gamepad button —
        // but the joystick, Fire, Start, Pause and Reset all still arrive.
        let (buttons, axes, _) = pad("A5200_CONTROLLER");
        assert_eq!(buttons, 0b0111_0011, "Fire 1/2, Pause, Reset, Start only");
        assert_eq!(axes & 0b11, 0b11, "the analog joystick is present");
        assert_eq!(buttons & HAT, 0, "a 5200 pad has no d-pad");

        // Same six face buttons, opposite slots, from the shoulder region
        // alone: digital L/R push C and Z onto the triggers, analog L/R push
        // them onto the shoulders.
        let (standard, standard_axes, _) = pad("SAT_CONTROLLER");
        let (three_d, three_d_axes, _) = pad("SAT_3D_CONTROLLER");
        assert_eq!(standard & FACE, FACE);
        assert_eq!(three_d & FACE, FACE);
        assert_eq!(standard_axes & 0b11_0000, 0b11_0000, "C and Z on triggers");
        assert_eq!(standard_axes & 0b11, 0, "the standard pad has no stick");
        assert_ne!(three_d & (1 << 9), 0, "C on the left shoulder");
        assert_ne!(three_d & (1 << 10), 0, "Z on the right shoulder");
        assert_eq!(three_d_axes & 0b11, 0b11, "the 3D pad has a stick");
    }
}
