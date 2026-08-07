//! The SDL virtual gamepad, as a device.
//!
//! Not a consolespec inputspec: it describes no console's controller, it
//! describes SDL's ABI. That is also why it is a list rather than a document —
//! position *is* the wire index, and SDL's numbering is not something any
//! expansion of a description would produce. It numbers eleven buttons, then the
//! hat, then six more; consolespec expands all buttons before any directional,
//! orders a hat N/E/S/W where SDL orders it up/down/left/right, and emits one
//! element per *signed half* where SDL has one axis per degree of freedom.
//!
//! Naming the pad's controls with the same [`BindingKey`] vocabulary every other
//! device uses is what lets the projection be device-key → SDL-key, rather than
//! device-cluster → a slot vocabulary that exists nowhere else.

use consolespec::{Alignment, AnalogClass, BindingKey, Component, Direction, Sign, key};

use crate::layout::{SdlAxis, SdlButton};

/// Where one key of the virtual gamepad lives on the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Wire {
    Button(SdlButton),
    Axis(SdlAxis),
    /// A raw joystick axis past the gamepad mapping, by its index.
    RawAxis(u8),
    /// Sensors are enabled by type rather than occupying a position.
    Sensor(SdlSensor),
}

impl Wire {
    /// The position this occupies among buttons, or among axes. Sensors have
    /// none — they are enabled by type.
    pub const fn index(self) -> Option<u8> {
        match self {
            Self::Button(button) => Some(button.index()),
            Self::Axis(axis) => Some(axis.index()),
            Self::RawAxis(index) => Some(index),
            Self::Sensor(_) => None,
        }
    }
}

/// SDL models a device with two of one sensor — the Joy-Cons, or a Wii Remote
/// with a Nunchuk — as a left and a right instance. The numeric
/// `SDL_SensorType` values live in the hook, which owns SDL's constants.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SdlSensor {
    Accelerometer,
    Gyroscope,
    AccelerometerLeft,
    GyroscopeLeft,
    AccelerometerRight,
    GyroscopeRight,
}

/// Our name for each control of the SDL gamepad.
///
/// The pairing is the content. Which of our controls is SDL's south is a choice
/// — the only one here that is ours — and SDL's numbering is a consequence of
/// it, not the other way round. Writing it as pairs is what lets a consumer join
/// on the control rather than count, which matters because SDL appends to its
/// enum: `MISC2`..`MISC6` already exist in newer headers than some of the
/// emulators we generate for.
pub static BUTTONS: [(SdlButton, BindingKey); 21] = [
    (SdlButton::South, key!(button.a)),
    (SdlButton::East, key!(button.b)),
    (SdlButton::West, key!(button.x)),
    (SdlButton::North, key!(button.y)),
    (SdlButton::Back, key!(button.select)),
    (SdlButton::Guide, key!(button.guide)),
    (SdlButton::Start, key!(button.start)),
    (SdlButton::LeftStick, key!(button.clickl)),
    (SdlButton::RightStick, key!(button.clickr)),
    (SdlButton::LeftShoulder, key!(button.l)),
    (SdlButton::RightShoulder, key!(button.r)),
    (SdlButton::DpadUp, key!(directional.n)),
    (SdlButton::DpadDown, key!(directional.s)),
    (SdlButton::DpadLeft, key!(directional.w)),
    (SdlButton::DpadRight, key!(directional.e)),
    (SdlButton::Misc1, key!(button.0)),
    (SdlButton::RightPaddle1, key!(button.1)),
    (SdlButton::LeftPaddle1, key!(button.2)),
    (SdlButton::RightPaddle2, key!(button.3)),
    (SdlButton::LeftPaddle2, key!(button.4)),
    (SdlButton::Touchpad, key!(button.5)),
];

/// Our name for each `SDL_GAMEPAD_AXIS_*`.
///
/// Each names its positive half, because a [`BindingKey`] addresses a signed
/// component and an SDL axis is one degree of freedom; the negative half is the
/// same axis, so [`wire`] matches either. A stick's `y+` is *up*, which is SDL's
/// negative Y — the two conventions meet here and nowhere else.
pub static AXES: [(SdlAxis, BindingKey); 6] = [
    (SdlAxis::LeftX, key!(analog.stick.left.x+)),
    (SdlAxis::LeftY, key!(analog.stick.left.y+)),
    (SdlAxis::RightX, key!(analog.stick.right.x+)),
    (SdlAxis::RightY, key!(analog.stick.right.y+)),
    (SdlAxis::LeftTrigger, key!(trigger.left)),
    (SdlAxis::RightTrigger, key!(trigger.right)),
];

/// The raw joystick axes snowglass adds past `SDL_GAMEPAD_AXIS_COUNT` for an
/// absolute pointer. They are not gamepad axes, so they have no `SdlAxis` and no
/// emulator names them; only one reading raw axes sees them.
pub static POINTER: [(u8, BindingKey); 2] = [(6, key!(pointer.x+)), (7, key!(pointer.y+))];

/// One signed half of an axis normalized to its positive twin, so that
/// `analog.stick.right.y-` and `analog.stick.right.y+` find the same axis.
fn positive(key: BindingKey) -> BindingKey {
    match key {
        BindingKey::Analog {
            class,
            alignment,
            component,
        } => BindingKey::Analog {
            class,
            alignment,
            component: Component {
                axis: component.axis,
                sign: Sign::Positive,
            },
        },
        BindingKey::Pointer {
            alignment,
            component: Some(component),
        } => BindingKey::Pointer {
            alignment,
            component: Some(Component {
                axis: component.axis,
                sign: Sign::Positive,
            }),
        },
        other => other,
    }
}

/// A sensor is enabled by class and side rather than placed at an index, so it
/// is derived from the key instead of being looked up in a table.
fn sensor(key: BindingKey) -> Option<SdlSensor> {
    let BindingKey::Analog {
        class, alignment, ..
    } = key
    else {
        return None;
    };
    Some(match (class, alignment) {
        (AnalogClass::Accelerometer, None) => SdlSensor::Accelerometer,
        (AnalogClass::Accelerometer, Some(Alignment::Left)) => SdlSensor::AccelerometerLeft,
        (AnalogClass::Accelerometer, Some(Alignment::Right)) => SdlSensor::AccelerometerRight,
        (AnalogClass::Gyroscope, None) => SdlSensor::Gyroscope,
        (AnalogClass::Gyroscope, Some(Alignment::Left)) => SdlSensor::GyroscopeLeft,
        (AnalogClass::Gyroscope, Some(Alignment::Right)) => SdlSensor::GyroscopeRight,
        _ => return None,
    })
}

/// The pad controls a *composite* control of the pad resolves to.
///
/// SDL's hat is four buttons, so an eight-way stick's north-east is the pad's
/// up *and* right held together; an absolute pointer is the pair of raw axes.
/// Both are controls the pad genuinely has — they are simply not one wire
/// location each, which is the case a cluster-level slot used to paper over.
pub fn decompose(key: BindingKey) -> Option<[BindingKey; 2]> {
    // Only the pad's own spelling: the hat and the pointer are unaligned, and
    // an aligned control reaches them through the device's primary cluster, not
    // by matching here. Two mice must not both claim the one pointer.
    match key {
        BindingKey::Directional {
            alignment: None,
            direction,
        } => {
            let (vertical, horizontal) = match direction {
                Direction::Ne => (Direction::N, Direction::E),
                Direction::Se => (Direction::S, Direction::E),
                Direction::Sw => (Direction::S, Direction::W),
                Direction::Nw => (Direction::N, Direction::W),
                _ => return None,
            };
            Some([
                BindingKey::Directional {
                    alignment: None,
                    direction: vertical,
                },
                BindingKey::Directional {
                    alignment: None,
                    direction: horizontal,
                },
            ])
        }
        // The absolute position is both raw axes at once.
        BindingKey::Pointer {
            alignment: None,
            component: None,
        } => Some([key!(pointer.x+), key!(pointer.y+)]),
        _ => None,
    }
}

/// Whether the pad has this control at all, as one wire location or as a
/// composite of two.
pub fn has(key: BindingKey) -> bool {
    wire(key).is_some() || decompose(key).is_some()
}

/// Where a key of the virtual gamepad lands, or `None` if the control is not one
/// wire location — either the pad does not have it, or it is a composite that
/// [`decompose`] splits.
pub fn wire(key: BindingKey) -> Option<Wire> {
    if let Some((button, _)) = BUTTONS.iter().find(|(_, candidate)| *candidate == key) {
        return Some(Wire::Button(*button));
    }
    let axis = positive(key);
    if let Some((sdl, _)) = AXES.iter().find(|(_, candidate)| *candidate == axis) {
        return Some(Wire::Axis(*sdl));
    }
    if let Some((index, _)) = POINTER.iter().find(|(_, candidate)| *candidate == axis) {
        return Some(Wire::RawAxis(*index));
    }
    sensor(key).map(Wire::Sensor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{SdlAxis, SdlButton};

    #[test]
    fn the_wire_order_is_sdls_own() {
        // The three places SDL's numbering and consolespec's expansion disagree,
        // pinned so a reordering of either is caught here rather than by an
        // emulator reading the wrong button.
        assert_eq!(wire(key!(button.a)), Some(Wire::Button(SdlButton::South)));
        assert_eq!(wire(key!(button.r)), Some(Wire::Button(SdlButton::RightShoulder)));
        // The hat interrupts the buttons, and runs up/down/left/right.
        assert_eq!(wire(key!(directional.n)), Some(Wire::Button(SdlButton::DpadUp)));
        assert_eq!(wire(key!(directional.s)), Some(Wire::Button(SdlButton::DpadDown)));
        assert_eq!(wire(key!(directional.w)), Some(Wire::Button(SdlButton::DpadLeft)));
        assert_eq!(wire(key!(directional.e)), Some(Wire::Button(SdlButton::DpadRight)));
        assert_eq!(wire(key!(button.0)), Some(Wire::Button(SdlButton::Misc1)));
        assert_eq!(wire(key!(button.5)), Some(Wire::Button(SdlButton::Touchpad)));
    }

    #[test]
    fn both_halves_of_an_axis_find_it() {
        assert_eq!(wire(key!(analog.stick.left.x+)), Some(Wire::Axis(SdlAxis::LeftX)));
        assert_eq!(wire(key!(analog.stick.left.x-)), Some(Wire::Axis(SdlAxis::LeftX)));
        assert_eq!(wire(key!(analog.stick.right.y-)), Some(Wire::Axis(SdlAxis::RightY)));
        assert_eq!(wire(key!(trigger.right)), Some(Wire::Axis(SdlAxis::RightTrigger)));
        assert_eq!(wire(key!(pointer.y-)), Some(Wire::RawAxis(7)));
    }

    #[test]
    fn sensors_are_placed_by_class_and_side() {
        assert_eq!(
            wire(key!(analog.gyroscope.z+)),
            Some(Wire::Sensor(SdlSensor::Gyroscope))
        );
        assert_eq!(
            wire(key!(analog.accelerometer.left.x-)),
            Some(Wire::Sensor(SdlSensor::AccelerometerLeft))
        );
        assert_eq!(
            wire(key!(analog.accelerometer.right.y+)),
            Some(Wire::Sensor(SdlSensor::AccelerometerRight))
        );
        // A side SDL has no instance for is not a sensor slot.
        assert_eq!(wire(key!(analog.gyroscope.front.x+)), None);
    }

    #[test]
    fn composite_controls_split_into_the_pad_it_actually_has() {
        // SDL's hat has no diagonal; north-east is up and right together.
        assert_eq!(
            decompose(key!(directional.ne)),
            Some([key!(directional.n), key!(directional.e)])
        );
        // An aligned control is not the pad's own; it reaches the hat through
        // the device's primary cluster or not at all.
        assert_eq!(decompose(key!(directional.left.sw)), None);
        assert_eq!(
            decompose(key!(pointer)),
            Some([key!(pointer.x+), key!(pointer.y+)])
        );
        // A cardinal is one button, not a composite.
        assert_eq!(decompose(key!(directional.n)), None);
        assert!(has(key!(directional.ne)) && has(key!(pointer)));
    }

    #[test]
    fn the_pad_does_not_have_what_it_does_not_have() {
        for absent in [
            key!(button.c),
            key!(button.z),
            key!(button.6),
            key!(analog.rotary.x+),
            key!(analog.slider.x+),
            key!(directional.left.n),
            key!(rumble.big),
            key!(touchscreen),
        ] {
            assert_eq!(wire(absent), None, "{absent}");
        }
    }

    /// `GamepadButton` — what the data plane sends — and SDL's own numbering
    /// are two spellings of one ABI. Nothing else checks that they agree, and if
    /// they ever did not the hook would advertise one button in its mask and the
    /// data plane would send another.
    #[test]
    fn the_wire_enum_agrees_with_sdls_numbering() {
        use crate::gamepad::GamepadButton;
        assert_eq!(GamepadButton::ALL.len(), BUTTONS.len());
        for (index, button) in GamepadButton::ALL.iter().enumerate() {
            let (sdl, _) = BUTTONS[index];
            assert_eq!(usize::from(sdl.index()), index, "{button:?} vs {sdl:?}");
            assert_eq!(*button as usize, index, "{button:?}");
        }
    }

    #[test]
    fn every_control_is_paired_exactly_once() {
        // A duplicate on either side would silently shadow, since `wire` and
        // the projection's identity default both take the first hit.
        for (sdl, key) in BUTTONS {
            assert_eq!(wire(key), Some(Wire::Button(sdl)), "{key}");
        }
        for (sdl, key) in AXES {
            assert_eq!(wire(key), Some(Wire::Axis(sdl)), "{key}");
        }
        for (index, key) in POINTER {
            assert_eq!(wire(key), Some(Wire::RawAxis(index)), "{key}");
        }
        let mut keys: Vec<_> = BUTTONS
            .iter()
            .map(|(_, key)| *key)
            .chain(AXES.iter().map(|(_, key)| *key))
            .collect();
        keys.sort();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count, "a control is named twice");
    }
}
