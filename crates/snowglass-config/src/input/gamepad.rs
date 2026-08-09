//! Identity of the SDL virtual gamepad injected by Snowglass.

use consolespec::{Alignment, AnalogClass, BindingKey, ButtonElement, Direction, key};
use crc::{CRC_16_ARC, Crc};
pub use uuid::Uuid;

/// The fixed SDL gamepad presented to emulators by the Snowglass input hook.
pub struct VirtualPad;

impl VirtualPad {
    pub const GAMEPAD_NAME: &str = "Snowglass Virtual Gamepad";

    /// The SDL GUID generated for Snowglass's vendor/product-less virtual pad.
    pub const GAMEPAD_GUID: Uuid = const {
        const SDL_CRC: Crc<u16> = Crc::<u16>::new(&CRC_16_ARC);

        let mut digest = SDL_CRC.digest();
        digest.update(Self::GAMEPAD_NAME.as_bytes());
        let crc = digest.finalize();

        let mut bytes = [0u8; 16];
        bytes[0] = 0xff; // SDL_HARDWARE_BUS_VIRTUAL, little endian.
        bytes[2] = crc as u8;
        bytes[3] = (crc >> 8) as u8;

        let name = Self::GAMEPAD_NAME.as_bytes();
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

    pub const fn has(key: BindingKey) -> bool {
        if Self::decompose(key).is_some() {
            return true;
        }
        match key {
            BindingKey::Button(button) => matches!(
                button,
                ButtonElement::A
                    | ButtonElement::B
                    | ButtonElement::X
                    | ButtonElement::Y
                    | ButtonElement::Select
                    | ButtonElement::Guide
                    | ButtonElement::Start
                    | ButtonElement::ClickL
                    | ButtonElement::ClickR
                    | ButtonElement::L
                    | ButtonElement::R
                    | ButtonElement::Numbered(0..=5)
            ),
            BindingKey::Directional {
                alignment: None,
                direction,
            } => matches!(
                direction,
                Direction::N | Direction::S | Direction::W | Direction::E
            ),
            BindingKey::Analog {
                class: AnalogClass::Stick,
                alignment: Some(Alignment::Left | Alignment::Right),
                ..
            } => true,
            BindingKey::Analog {
                class: AnalogClass::Accelerometer | AnalogClass::Gyroscope,
                alignment: None | Some(Alignment::Left | Alignment::Right),
                ..
            } => true,
            BindingKey::Trigger {
                alignment: Alignment::Left | Alignment::Right,
            } => true,
            BindingKey::Pointer {
                alignment: None,
                component: Some(_),
            } => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_gamepad_guid_is_stable() {
        assert_eq!(
            VirtualPad::GAMEPAD_GUID.simple().to_string(),
            "ff009378536e6f77676c617373007601"
        );
        assert_eq!(VirtualPad::GAMEPAD_NAME, "Snowglass Virtual Gamepad");
        assert!(VirtualPad::has(key!(button.a)));
        assert_eq!(
            VirtualPad::decompose(key!(directional.ne)),
            Some([key!(directional.n), key!(directional.e)])
        );
    }
}
