use consolespec::input::{Alignment, AnalogClass, BindingKey, Component, Sign};
pub use polymer_config::input::gamepad::StandardGamepad;
use polymer_input::GamepadButton;

/// A channel in Polymer's backend-neutral controller report.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Control {
    Button(u8),
    Axis(u8),
    Sensor(Sensor),
}

impl Control {
    pub const fn index(self) -> Option<u8> {
        match self {
            Self::Button(index) | Self::Axis(index) => Some(index),
            Self::Sensor(_) => None,
        }
    }

    pub fn from_key(key: BindingKey) -> Option<Self> {
        if let Some(index) = StandardGamepad::BUTTONS
            .iter()
            .position(|candidate| *candidate == key)
        {
            return Some(Self::Button(index as u8));
        }
        let positive = match key {
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
        if let Some(index) = StandardGamepad::AXES
            .iter()
            .position(|candidate| *candidate == positive)
        {
            return Some(Self::Axis(index as u8));
        }
        if let Some(index) = StandardGamepad::POINTER_AXES
            .iter()
            .position(|candidate| *candidate == positive)
        {
            return Some(Self::Axis(StandardGamepad::AXES.len() as u8 + index as u8));
        }
        let BindingKey::Analog {
            class, alignment, ..
        } = key
        else {
            return None;
        };
        matches!(class, AnalogClass::Accelerometer | AnalogClass::Gyroscope)
            .then_some(Sensor::new(class, alignment))
            .filter(|sensor| sensor.index().is_some())
            .map(Self::Sensor)
    }

    pub fn binding(button: GamepadButton) -> BindingKey {
        StandardGamepad::BUTTONS[button as usize]
    }
}

/// A sensor identified using consolespec's semantic vocabulary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Sensor {
    pub class: AnalogClass,
    pub alignment: Option<Alignment>,
}

impl Sensor {
    pub const ALL: [Self; 6] = [
        Self::new(AnalogClass::Accelerometer, None),
        Self::new(AnalogClass::Gyroscope, None),
        Self::new(AnalogClass::Accelerometer, Some(Alignment::Left)),
        Self::new(AnalogClass::Gyroscope, Some(Alignment::Left)),
        Self::new(AnalogClass::Accelerometer, Some(Alignment::Right)),
        Self::new(AnalogClass::Gyroscope, Some(Alignment::Right)),
    ];

    pub const fn new(class: AnalogClass, alignment: Option<Alignment>) -> Self {
        Self { class, alignment }
    }

    pub fn index(self) -> Option<u8> {
        Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .map(|index| index as u8)
    }
}
