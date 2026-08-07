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

/// Where one key of the virtual gamepad lives on the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Wire {
    /// Index into [`BUTTONS`], which is `SDL_GAMEPAD_BUTTON_*`.
    Button(u8),
    /// Index into [`AXES`], which is `SDL_GAMEPAD_AXIS_*` and then the two raw
    /// axes snowglass adds for an absolute pointer.
    Axis(u8),
    /// Sensors are enabled by type rather than occupying an index.
    Sensor(SdlSensor),
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

/// `SDL_GAMEPAD_BUTTON_*`, in value order. The hat sits at 11-14, between the
/// standard buttons and the spares, which is the interleaving no expansion of a
/// description reproduces.
pub static BUTTONS: [BindingKey; 21] = [
    key!(button.a),      // south
    key!(button.b),      // east
    key!(button.x),      // west
    key!(button.y),      // north
    key!(button.select), // back
    key!(button.guide),
    key!(button.start),
    key!(button.clickl), // left stick click
    key!(button.clickr), // right stick click
    key!(button.l),      // left shoulder
    key!(button.r),      // right shoulder
    key!(directional.n), // dpad up
    key!(directional.s), // dpad down
    key!(directional.w), // dpad left
    key!(directional.e), // dpad right
    key!(button.0),      // misc1
    key!(button.1),      // right paddle 1
    key!(button.2),      // left paddle 1
    key!(button.3),      // right paddle 2
    key!(button.4),      // left paddle 2
    key!(button.5),      // touchpad click
];

/// `SDL_GAMEPAD_AXIS_*`, then the two raw joystick axes snowglass presents for
/// an absolute pointer. Each entry names its positive half because a
/// [`BindingKey`] addresses a signed component; the negative half is the same
/// axis, so [`wire`] matches either.
pub static AXES: [BindingKey; 8] = [
    key!(analog.stick.left.x+),
    key!(analog.stick.left.y+),
    key!(analog.stick.right.x+),
    key!(analog.stick.right.y+),
    key!(trigger.left),
    key!(trigger.right),
    key!(pointer.x+),
    key!(pointer.y+),
];

/// The two raw axes outside `SDL_GAMEPAD_AXIS_COUNT`. Only an emulator reading
/// raw joystick axes sees them, and they are present on every pad.
pub const POINTER_AXES: [u8; 2] = [6, 7];

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
    if let Some(index) = BUTTONS.iter().position(|candidate| *candidate == key) {
        return Some(Wire::Button(index as u8));
    }
    let axis = positive(key);
    if let Some(index) = AXES.iter().position(|candidate| *candidate == axis) {
        return Some(Wire::Axis(index as u8));
    }
    sensor(key).map(Wire::Sensor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_order_is_sdls_own() {
        // The three places SDL's numbering and consolespec's expansion disagree,
        // pinned so a reordering of either is caught here rather than by an
        // emulator reading the wrong button.
        assert_eq!(wire(key!(button.a)), Some(Wire::Button(0)));
        assert_eq!(wire(key!(button.r)), Some(Wire::Button(10)));
        // The hat interrupts the buttons, and runs up/down/left/right.
        assert_eq!(wire(key!(directional.n)), Some(Wire::Button(11)));
        assert_eq!(wire(key!(directional.s)), Some(Wire::Button(12)));
        assert_eq!(wire(key!(directional.w)), Some(Wire::Button(13)));
        assert_eq!(wire(key!(directional.e)), Some(Wire::Button(14)));
        assert_eq!(wire(key!(button.0)), Some(Wire::Button(15)));
        assert_eq!(wire(key!(button.5)), Some(Wire::Button(20)));
    }

    #[test]
    fn both_halves_of_an_axis_find_it() {
        assert_eq!(wire(key!(analog.stick.left.x+)), Some(Wire::Axis(0)));
        assert_eq!(wire(key!(analog.stick.left.x-)), Some(Wire::Axis(0)));
        assert_eq!(wire(key!(analog.stick.right.y-)), Some(Wire::Axis(3)));
        assert_eq!(wire(key!(trigger.right)), Some(Wire::Axis(5)));
        assert_eq!(wire(key!(pointer.y-)), Some(Wire::Axis(7)));
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

    /// The wire enum and this list are two spellings of one ABI. Nothing else
    /// checks that they agree, and if they ever did not the hook would advertise
    /// one button in its mask and the data plane would send another.
    #[test]
    fn wire_order_matches_the_pad() {
        use crate::gamepad::GamepadButton;
        assert_eq!(GamepadButton::ALL.len(), BUTTONS.len());
        for (index, button) in GamepadButton::ALL.iter().enumerate() {
            assert_eq!(
                wire(BUTTONS[index]),
                Some(Wire::Button(index as u8)),
                "{button:?}"
            );
            assert_eq!(*button as usize, index, "{button:?}");
        }
    }

    #[test]
    fn every_key_is_placed_exactly_once() {
        // A duplicate would silently shadow, since `wire` takes the first hit.
        for (index, key) in BUTTONS.iter().enumerate() {
            assert_eq!(wire(*key), Some(Wire::Button(index as u8)), "{key}");
        }
        for (index, key) in AXES.iter().enumerate() {
            assert_eq!(wire(*key), Some(Wire::Axis(index as u8)), "{key}");
        }
    }
}
