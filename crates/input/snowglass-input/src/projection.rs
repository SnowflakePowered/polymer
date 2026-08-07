//! Projection of a device's controls onto SDL's virtual gamepad.
//!
//! This is mapping, not description. A device says what controls it has; this
//! decides which of the SDL pad's controls carry them. Both sides are
//! [`BindingKey`]s — the left of the device, the right of [`crate::sdl`]'s pad —
//! so there is no third vocabulary of slot names, and the same table read
//! backwards says which of *our* controls a physical pad's button is.
//!
//! Keeping it in one shared table means "which physical button is C-Up" is the
//! same in every emulator, and that the six-slot overflow budget can be checked
//! globally — a per-plugin table could not tell you that two plugins had
//! allocated the same slot differently.
//!
//! The allocation follows SDL's own `gamecontrollerdb`: unused standard slots
//! are filled before any spare. A Wii Remote's 1 and 2 become X and Y because a
//! Wii Remote has no X/Y face buttons; a Saturn pad's C and Z become shoulders
//! because its shoulder slots are free.

use consolespec::BindingKey;
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

use crate::layout::InputLayout;
use crate::sdl;

pub const EMBEDDED_SDL_PROJECTION: &str = include_str!("../projection/SDL.proj.toml");

/// A way off the pad. Some controls are not gamepad controls at all, and saying
/// *which* transport carries them is what lets one be routed later.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Transport {
    /// A 5200's or an Intellivision's keypad is a keypad; the emulator reads it
    /// as keys, not as pad buttons.
    Keyboard,
}

impl Transport {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "keyboard" => Some(Self::Keyboard),
            _ => None,
        }
    }
}

impl fmt::Display for Transport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Keyboard => formatter.write_str("keyboard"),
        }
    }
}

/// Where one of a device's controls goes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    /// The SDL pad control that carries it.
    Key(BindingKey),
    /// Two pad controls at once: an eight-way stick's north-east is the pad's
    /// up *and* right, and an absolute pointer is both raw axes.
    Both(BindingKey, BindingKey),
    /// Off the pad, but reached: the named transport carries it instead.
    Transport(Transport),
    /// Off the pad and reached by nothing. SDL presents one pointer, so a device
    /// with two mice can only place one. Distinct from an absent entry, which is
    /// an oversight rather than a decision.
    None,
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct Document(HashMap<String, HashMap<String, String>>);

#[derive(Clone, Debug)]
pub struct Projection {
    /// Layout id -> device key -> where it goes.
    layouts: HashMap<String, HashMap<BindingKey, Target>>,
}

impl Projection {
    pub fn embedded() -> Result<Self, String> {
        Self::from_toml(EMBEDDED_SDL_PROJECTION)
    }

    /// The embedded table, parsed once.
    ///
    /// Like the layouts it maps, this is fixed data compiled into the binary, so
    /// there is nothing to own and no reason for a caller deep in the data plane
    /// to have been handed one.
    pub fn shared() -> &'static Self {
        static SHARED: OnceLock<Projection> = OnceLock::new();
        SHARED.get_or_init(|| {
            Self::embedded()
                .unwrap_or_else(|error| panic!("the SDL projection is unusable: {error}"))
        })
    }

    pub fn from_toml(source: &str) -> Result<Self, String> {
        let document: Document =
            toml::from_str(source).map_err(|error| format!("invalid projection: {error}"))?;
        let mut layouts = HashMap::new();
        for (layout, table) in document.0 {
            let mut entries = HashMap::new();
            for (key, target) in table {
                let key = BindingKey::from_str(&key)
                    .map_err(|error| format!("projection {layout}: {error}"))?;
                let target = if target == "none" {
                    Target::None
                } else if let Some(transport) = Transport::parse(&target) {
                    Target::Transport(transport)
                } else {
                    let target = BindingKey::from_str(&target)
                        .map_err(|error| format!("projection {layout}.{key}: {error}"))?;
                    // A target the pad does not have could never fire, and is
                    // the mistake this file is most able to make.
                    if !sdl::has(target) {
                        return Err(format!(
                            "projection {layout}.{key}: the SDL pad has no {target}"
                        ));
                    }
                    Target::Key(target)
                };
                entries.insert(key, target);
            }
            layouts.insert(layout, entries);
        }
        Ok(Self { layouts })
    }

    /// Where one control of `layout` lands.
    ///
    /// An explicit entry wins. Otherwise the control keeps its own name, if the
    /// pad has it — which is what lets the table list only the differences.
    /// Identity is resolved against the device, so a lone D-pad's
    /// `directional.left.n` reaches the pad's `directional.n`, while a *second*
    /// directional does not: the N64's C-buttons are not its D-pad, and must
    /// say where they go.
    pub fn target(&self, layout: &InputLayout, key: BindingKey) -> Option<Target> {
        if let Some(target) = self
            .layouts
            .get(&layout.id)
            .and_then(|table| table.get(&key))
        {
            return Some(*target);
        }
        // A control the pad has under the same name keeps it.
        if sdl::has(key) {
            return Some(Self::place(key));
        }
        // Otherwise the device's *primary* cluster of that kind answers to the
        // pad's unqualified name for it, and no other does.
        let unaligned = key.unaligned();
        if layout.resolve(unaligned) == Some(key) && sdl::has(unaligned) {
            return Some(Self::place(unaligned));
        }
        None
    }

    /// One pad control, or the two a composite splits into.
    fn place(target: BindingKey) -> Target {
        match sdl::decompose(target) {
            Some([first, second]) => Target::Both(first, second),
            None => Target::Key(target),
        }
    }

    /// Every control of `layout` that reaches neither the pad nor a declared
    /// transport. A binding for one of these could never fire.
    pub fn unprojected(&self, layout: &InputLayout) -> Vec<BindingKey> {
        layout
            .elements
            .iter()
            .map(|element| element.key)
            .filter(|key| self.target(layout, *key).is_none())
            .collect()
    }

    pub fn layout_names(&self) -> impl Iterator<Item = &str> {
        self.layouts.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{SdlAxis, SdlButton};
    use crate::sdl::{SdlSensor, Wire};
    use consolespec::key;

    fn placed(layout: &str, control: BindingKey) -> Option<Wire> {
        let layout = InputLayout::get(layout).unwrap();
        match Projection::embedded().unwrap().target(layout, control) {
            Some(Target::Key(target)) => sdl::wire(target),
            _ => None,
        }
    }

    #[test]
    fn embedded_projection_parses() {
        let projection = Projection::embedded().unwrap();
        assert!(projection.layout_names().count() > 20);
    }

    #[test]
    fn every_layout_is_fully_projected() {
        let projection = Projection::embedded().unwrap();
        let mut gaps = Vec::new();
        for layout in InputLayout::all() {
            for key in projection.unprojected(layout) {
                // Output and non-gamepad controls have no SDL input slot by
                // design; everything a binding can name must reach somewhere.
                if matches!(key, BindingKey::Rumble(_) | BindingKey::Peripheral { .. }) {
                    continue;
                }
                gaps.push(format!("{}: {key}", layout.id));
            }
        }
        assert!(gaps.is_empty(), "controls with no projection: {gaps:#?}");
    }

    #[test]
    fn identity_covers_what_the_pad_already_has() {
        // Nothing in the table says so; both sides simply agree.
        assert_eq!(
            placed("XBOX_CONTROLLER", key!(button.a)),
            Some(Wire::Button(SdlButton::South))
        );
        assert_eq!(
            placed("XBOX_CONTROLLER", key!(analog.stick.right.y-)),
            Some(Wire::Axis(SdlAxis::RightY))
        );
    }

    #[test]
    fn only_the_primary_cluster_answers_to_an_unaligned_key() {
        // Both of the N64's directionals are aligned, so neither is identical
        // to the pad's; the D-pad reaches the hat by resolution, and the
        // C-buttons are placed by name.
        assert_eq!(
            placed("N64_CONTROLLER", key!(directional.left.n)),
            Some(Wire::Button(SdlButton::DpadUp))
        );
        assert_eq!(
            placed("N64_CONTROLLER", key!(directional.right.n)),
            Some(Wire::Axis(SdlAxis::RightY))
        );
    }

    #[test]
    fn saturn_pads_differ_by_what_else_they_carry() {
        // The standard pad's L/R are digital shoulders, so C and Z spill to the
        // triggers. The 3D pad's L/R are analog triggers, so C and Z take the
        // shoulders instead. Same buttons, opposite slots, no hand-written rule.
        assert_eq!(
            placed("SAT_CONTROLLER", key!(button.c)),
            Some(Wire::Axis(SdlAxis::LeftTrigger))
        );
        assert_eq!(
            placed("SAT_CONTROLLER", key!(button.z)),
            Some(Wire::Axis(SdlAxis::RightTrigger))
        );
        assert_eq!(
            placed("SAT_3D_CONTROLLER", key!(button.c)),
            Some(Wire::Button(SdlButton::LeftShoulder))
        );
        assert_eq!(
            placed("SAT_3D_CONTROLLER", key!(button.z)),
            Some(Wire::Button(SdlButton::RightShoulder))
        );
    }

    #[test]
    fn a_nunchuk_carries_two_accelerometers() {
        const NUNCHUK: &str = "WII_REMOTE_NUNCHUCK_CONTROLLER";
        assert_eq!(
            placed(NUNCHUK, key!(analog.accelerometer.right.x+)),
            Some(Wire::Sensor(SdlSensor::AccelerometerRight))
        );
        assert_eq!(
            placed(NUNCHUK, key!(analog.accelerometer.left.x+)),
            Some(Wire::Sensor(SdlSensor::AccelerometerLeft))
        );
        assert_eq!(
            placed(NUNCHUK, key!(analog.gyroscope.x+)),
            Some(Wire::Sensor(SdlSensor::Gyroscope))
        );
    }

    #[test]
    fn a_keypad_says_which_transport_carries_it() {
        let layout = InputLayout::get("A5200_CONTROLLER").unwrap();
        let projection = Projection::embedded().unwrap();
        // Not merely absent from the pad — routed, and the table says where.
        assert_eq!(
            projection.target(layout, key!(button.0)),
            Some(Target::Transport(Transport::Keyboard))
        );
        assert!(!projection.unprojected(layout).contains(&key!(button.0)));
        // The 5200's stick and its Fire/Start/Pause/Reset are pad controls.
        assert_eq!(
            placed("A5200_CONTROLLER", key!(button.start)),
            Some(Wire::Button(SdlButton::Start))
        );
    }

    #[test]
    fn a_control_nothing_reaches_is_declared_too() {
        // SDL presents one pointer, so the second Joy-Con's mouse goes nowhere.
        let layout = InputLayout::get("SWITCH_2_JOYCONS_CONTROLLER").unwrap();
        let projection = Projection::embedded().unwrap();
        assert_eq!(
            projection.target(layout, key!(pointer.right)),
            Some(Target::None)
        );
        // Declared, so it is not a gap — unlike the rumble motors and
        // touchscreen, which have no pad input slot by design.
        assert!(
            !projection
                .unprojected(layout)
                .contains(&key!(pointer.right))
        );
        assert!(
            !projection
                .unprojected(layout)
                .contains(&key!(pointer.right.x+))
        );
    }

    #[test]
    fn a_target_the_pad_lacks_is_rejected_at_load() {
        let error =
            Projection::from_toml("[XBOX_CONTROLLER]\n\"button.a\" = \"button.c\"\n").unwrap_err();
        assert!(error.contains("the SDL pad has no button.c"), "{error}");
    }
}

#[cfg(test)]
mod smoke {
    use super::*;

    /// The pad each layout actually presents. These were read off the
    /// projection and checked by hand against the physical devices; they exist
    /// to catch a change in the allocation rule, which has silently mis-placed
    /// controls twice — two accelerometers colliding on one sensor slot, and a
    /// second directional inheriting the hat instead of the right stick.
    #[test]
    fn representative_layouts_project_as_expected() {
        let projection = Projection::embedded().unwrap();
        let pad = |id: &str| {
            let out = InputLayout::get(id).unwrap().sdl_output(&projection);
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
        assert!(sensors.contains(&crate::sdl::SdlSensor::AccelerometerLeft));
        assert!(sensors.contains(&crate::sdl::SdlSensor::AccelerometerRight));
        assert!(sensors.contains(&crate::sdl::SdlSensor::Gyroscope));
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
