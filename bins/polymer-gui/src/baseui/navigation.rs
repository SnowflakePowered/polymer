use eframe::egui;
use sdl3::{
    event::Event,
    gamepad::{Axis, Button, Gamepad},
};

const STICK_THRESHOLD: i16 = 16000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum NavAction {
    Up,
    Down,
    PrevTab,
    NextTab,
    Confirm,
    Cancel,
}

impl NavAction {
    pub(super) fn from_keyboard(input: &egui::InputState) -> Vec<Self> {
        let bindings = [
            (egui::Key::ArrowUp, Self::Up),
            (egui::Key::ArrowDown, Self::Down),
            (egui::Key::ArrowLeft, Self::PrevTab),
            (egui::Key::ArrowRight, Self::NextTab),
            (egui::Key::Enter, Self::Confirm),
            (egui::Key::Escape, Self::Cancel),
        ];
        let mut actions: Vec<Self> = bindings
            .into_iter()
            .filter(|(key, _)| input.key_pressed(*key))
            .map(|(_, action)| action)
            .collect();
        if input.key_pressed(egui::Key::Tab) {
            actions.push(if input.modifiers.shift { Self::PrevTab } else { Self::NextTab });
        }
        actions
    }

    fn from_button(button: Button) -> Option<Self> {
        match button {
            Button::DPadUp => Some(Self::Up),
            Button::DPadDown => Some(Self::Down),
            Button::DPadLeft | Button::LeftShoulder => Some(Self::PrevTab),
            Button::DPadRight | Button::RightShoulder => Some(Self::NextTab),
            Button::South => Some(Self::Confirm),
            Button::East => Some(Self::Cancel),
            _ => None,
        }
    }
}

pub(super) struct GamepadNavigation {
    _sdl: sdl3::Sdl,
    subsystem: sdl3::GamepadSubsystem,
    events: sdl3::EventPump,
    pads: Vec<Gamepad>,
    stick: [i8; 2],
}

impl GamepadNavigation {
    pub(super) fn new() -> Option<Self> {
        sdl3::hint::set("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
        let sdl = sdl3::init().ok()?;
        let subsystem = sdl.gamepad().ok()?;
        let events = sdl.event_pump().ok()?;
        let mut navigation = Self {
            _sdl: sdl,
            subsystem,
            events,
            pads: Vec::new(),
            stick: [0; 2],
        };
        for id in navigation.subsystem.gamepads().unwrap_or_default() {
            navigation.open(id);
        }
        Some(navigation)
    }

    fn open(&mut self, id: sdl3::joystick::JoystickId) {
        if let Ok(pad) = self.subsystem.open(id) {
            self.pads.push(pad);
        }
    }

    fn stick_action(&mut self, axis: Axis, value: i16) -> Option<NavAction> {
        let (slot, negative, positive) = match axis {
            Axis::LeftX => (0, NavAction::PrevTab, NavAction::NextTab),
            Axis::LeftY => (1, NavAction::Up, NavAction::Down),
            _ => return None,
        };
        let state = match value {
            v if v <= -STICK_THRESHOLD => -1,
            v if v >= STICK_THRESHOLD => 1,
            _ => 0,
        };
        let previous = std::mem::replace(&mut self.stick[slot], state);
        match (previous, state) {
            (p, -1) if p != -1 => Some(negative),
            (p, 1) if p != 1 => Some(positive),
            _ => None,
        }
    }

    pub(super) fn poll(&mut self) -> Vec<NavAction> {
        let events: Vec<Event> = self.events.poll_iter().take(64).collect();
        let mut actions = Vec::new();
        for event in events {
            match event {
                Event::ControllerDeviceAdded { which, .. } => {
                    self.open(sdl3::sys::joystick::SDL_JoystickID(which))
                }
                Event::ControllerDeviceRemoved { .. } => self.pads.retain(Gamepad::connected),
                Event::ControllerButtonDown { button, .. } => {
                    actions.extend(NavAction::from_button(button))
                }
                Event::ControllerAxisMotion { axis, value, .. } => {
                    actions.extend(self.stick_action(axis, value))
                }
                _ => {}
            }
        }
        actions
    }
}
