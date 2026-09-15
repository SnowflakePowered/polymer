//! The backend-neutral standard gamepad model used by Polymer projections.

use consolespec::input::{
    Alignment, AnalogClass, Axis, BindingKey, Component, Direction, Sign, key,
};
use crc::{CRC_16_ARC, Crc};
pub use uuid::Uuid;

/// The fixed controller report presented to input backends.
pub struct StandardGamepad;

impl StandardGamepad {
    pub const BUTTONS: [BindingKey; 21] = [
        key!(button.a),
        key!(button.b),
        key!(button.x),
        key!(button.y),
        key!(button.select),
        key!(button.guide),
        key!(button.start),
        key!(button.clickl),
        key!(button.clickr),
        key!(button.l),
        key!(button.r),
        key!(directional.n),
        key!(directional.s),
        key!(directional.w),
        key!(directional.e),
        key!(button.0),
        key!(button.1),
        key!(button.2),
        key!(button.3),
        key!(button.4),
        key!(button.5),
    ];

    pub const AXES: [BindingKey; 6] = [
        key!(analog.stick.left.x+),
        key!(analog.stick.left.y+),
        key!(analog.stick.right.x+),
        key!(analog.stick.right.y+),
        key!(trigger.left),
        key!(trigger.right),
    ];

    pub const POINTER_AXES: [BindingKey; 2] = [key!(pointer.x+), key!(pointer.y+)];

    pub const fn decompose(key: BindingKey) -> Option<[BindingKey; 2]> {
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
        if Self::decompose(key).is_some() {
            return true;
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
        if Self::BUTTONS.contains(&positive)
            || Self::AXES.contains(&positive)
            || Self::POINTER_AXES.contains(&positive)
        {
            return true;
        }
        matches!(
            key,
            BindingKey::Analog {
                class: AnalogClass::Accelerometer | AnalogClass::Gyroscope,
                alignment: None | Some(Alignment::Left | Alignment::Right),
                component: Component {
                    axis: Axis::X | Axis::Y | Axis::Z,
                    ..
                },
            }
        )
    }
}

/// SDL's stable identity for the standard gamepad.
pub struct SdlGamepadIdentity;

impl SdlGamepadIdentity {
    pub const NAME: &str = "Polymer Virtual Gamepad";

    pub const GUID: Uuid = const {
        const SDL_CRC: Crc<u16> = Crc::<u16>::new(&CRC_16_ARC);

        let mut digest = SDL_CRC.digest();
        digest.update(Self::NAME.as_bytes());
        let crc = digest.finalize();

        let mut bytes = [0u8; 16];
        bytes[0] = 0xff; // SDL_HARDWARE_BUS_VIRTUAL, little endian.
        bytes[2] = crc as u8;
        bytes[3] = (crc >> 8) as u8;

        let name = Self::NAME.as_bytes();
        let mut index = 0;
        // SDL leaves two bytes for the driver signature and copies at most nine
        // bytes plus a terminator into the remaining name field.
        while index < 9 && index < name.len() {
            bytes[4 + index] = name[index];
            index += 1;
        }
        bytes[14] = b'v';
        bytes[15] = 1; // SDL_JOYSTICK_TYPE_GAMEPAD.

        Uuid::from_bytes(bytes)
    };
}
