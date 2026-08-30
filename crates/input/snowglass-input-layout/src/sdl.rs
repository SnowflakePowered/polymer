use consolespec::input::{Alignment, AnalogClass, BindingKey, Component, Direction, Sign, key};

use crate::layout::{SdlAxis, SdlButton};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Wire {
    Button(SdlButton),
    Axis(SdlAxis),
    RawAxis(u8),
    Sensor(SdlSensor),
}

impl Wire {
    pub const fn index(self) -> Option<u8> {
        match self {
            Self::Button(button) => Some(button.index()),
            Self::Axis(axis) => Some(axis.index()),
            Self::RawAxis(index) => Some(index),
            Self::Sensor(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SdlSensor {
    Accelerometer,
    Gyroscope,
    AccelerometerLeft,
    GyroscopeLeft,
    AccelerometerRight,
    GyroscopeRight,
}

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

pub static AXES: [(SdlAxis, BindingKey); 6] = [
    (SdlAxis::LeftX, key!(analog.stick.left.x+)),
    (SdlAxis::LeftY, key!(analog.stick.left.y+)),
    (SdlAxis::RightX, key!(analog.stick.right.x+)),
    (SdlAxis::RightY, key!(analog.stick.right.y+)),
    (SdlAxis::LeftTrigger, key!(trigger.left)),
    (SdlAxis::RightTrigger, key!(trigger.right)),
];

pub static POINTER: [(u8, BindingKey); 2] = [(6, key!(pointer.x+)), (7, key!(pointer.y+))];

pub fn decompose(key: BindingKey) -> Option<[BindingKey; 2]> {
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
        BindingKey::Pointer {
            alignment: None,
            component: None,
        } => Some([key!(pointer.x+), key!(pointer.y+)]),
        _ => None,
    }
}

pub fn has(key: BindingKey) -> bool {
    wire(key).is_some() || decompose(key).is_some()
}

pub fn wire(key: BindingKey) -> Option<Wire> {
    if let Some((button, _)) = BUTTONS.iter().find(|(_, candidate)| *candidate == key) {
        return Some(Wire::Button(*button));
    }
    let axis = match key {
        BindingKey::Analog {
            class,
            alignment,
            component,
        } => BindingKey::Analog {
            class,
            alignment,
            component: Component::new(component.axis, Sign::Positive),
        },
        BindingKey::Pointer {
            alignment,
            component: Some(component),
        } => BindingKey::Pointer {
            alignment,
            component: Some(Component::new(component.axis, Sign::Positive)),
        },
        other => other,
    };
    if let Some((sdl, _)) = AXES.iter().find(|(_, candidate)| *candidate == axis) {
        return Some(Wire::Axis(*sdl));
    }
    if let Some((index, _)) = POINTER.iter().find(|(_, candidate)| *candidate == axis) {
        return Some(Wire::RawAxis(*index));
    }
    let BindingKey::Analog {
        class, alignment, ..
    } = key
    else {
        return None;
    };
    match (class, alignment) {
        (AnalogClass::Accelerometer, None) => Some(SdlSensor::Accelerometer),
        (AnalogClass::Accelerometer, Some(Alignment::Left)) => Some(SdlSensor::AccelerometerLeft),
        (AnalogClass::Accelerometer, Some(Alignment::Right)) => Some(SdlSensor::AccelerometerRight),
        (AnalogClass::Gyroscope, None) => Some(SdlSensor::Gyroscope),
        (AnalogClass::Gyroscope, Some(Alignment::Left)) => Some(SdlSensor::GyroscopeLeft),
        (AnalogClass::Gyroscope, Some(Alignment::Right)) => Some(SdlSensor::GyroscopeRight),
        _ => None,
    }
    .map(Wire::Sensor)
}
