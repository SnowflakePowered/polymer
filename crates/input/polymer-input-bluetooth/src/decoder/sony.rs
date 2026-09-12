use super::GamepadState;
use polymer_input::GamepadButton;

pub(super) struct SonyControls;

impl SonyControls {
    pub fn state(sticks: [u8; 4], triggers: [u8; 2], bits: u32) -> GamepadState {
        use GamepadButton::*;
        let sticks =
            sticks.map(|v| ((i32::from(v) - 128) * 32767 / 127).clamp(-32768, 32767) as i16);
        let mut state = GamepadState {
            left_stick: [sticks[0], sticks[1]],
            right_stick: [sticks[2], sticks[3]],
            left_trigger: u16::from(triggers[0]) * 257,
            right_trigger: u16::from(triggers[1]) * 257,
            ..Default::default()
        };
        for (bit, button) in [
            (4, X),
            (5, A),
            (6, B),
            (7, Y),
            (8, L),
            (9, R),
            (12, Select),
            (13, Start),
            (14, LeftStick),
            (15, RightStick),
            (16, Guide),
            (17, Touchpad),
            (18, Misc1),
        ] {
            if bits & (1 << bit) != 0 {
                state.buttons |= button.mask();
            }
        }
        let hats = [
            DpadUp.mask(),
            DpadUp.mask() | DpadRight.mask(),
            DpadRight.mask(),
            DpadRight.mask() | DpadDown.mask(),
            DpadDown.mask(),
            DpadDown.mask() | DpadLeft.mask(),
            DpadLeft.mask(),
            DpadLeft.mask() | DpadUp.mask(),
        ];
        state.buttons |= hats.get((bits & 15) as usize).copied().unwrap_or(0);
        state
    }
}

#[derive(Default)]
pub(super) struct SonyTouchState {
    primary: Option<u8>,
    last_pointer: [i16; 2],
    contacts: [super::TouchContact; 2],
}

impl SonyTouchState {
    pub fn apply(
        &mut self,
        touch: Option<&[polymer_bluetooth::devices::sony::protocol::TouchContact; 2]>,
        extent: [f32; 2],
        state: &mut GamepadState,
    ) {
        if let Some(touch) = touch {
            self.update(touch, extent, state);
        } else {
            state.pointer = self.last_pointer;
            state.touch_contacts = Some(self.contacts);
        }
    }

    fn update(
        &mut self,
        touch: &[polymer_bluetooth::devices::sony::protocol::TouchContact; 2],
        extent: [f32; 2],
        state: &mut GamepadState,
    ) {
        let contacts = std::array::from_fn(|i| {
            let contact = &touch[i];
            super::TouchContact {
                id: contact.id(),
                active: contact.active(),
                position: if contact.active() {
                    contact.position()
                } else {
                    [0; 2]
                },
            }
        });
        // Retain the same finger while both are down, regardless of slot order.
        let primary = contacts
            .iter()
            .find(|c| c.active && Some(c.id) == self.primary)
            .or_else(|| contacts.iter().find(|c| c.active));
        self.primary = primary.map(|c| c.id);
        if let Some(contact) = primary {
            self.last_pointer = std::array::from_fn(|axis| {
                let extent = extent[axis];
                super::pointer_axis(f32::from(contact.position[axis]) / extent)
            });
        }
        state.pointer = self.last_pointer;
        self.contacts = contacts;
        state.touch_contacts = Some(contacts);
    }
}
