use consolespec::input::{BindingKey, Component, Sign};
use snowglass_input::{GamepadReport, KeyCode};
use std::collections::{BTreeSet, HashMap};
use std::fmt;

use crate::layout::{
    EMBEDDED_DEFAULT_KEYBOARD_MAPPING, InputBinding, InputLayout, KeyboardMapping,
};
use crate::sdl::Wire;

#[derive(Clone, Debug, Default)]
pub struct KeySet(BTreeSet<String>);

impl KeySet {
    pub fn set(&mut self, code: &str, pressed: bool) {
        if pressed {
            self.0.insert(code.to_owned());
        } else {
            self.0.remove(code);
        }
    }

    pub fn contains(&self, code: KeyCode) -> bool {
        self.0.contains(code.as_str())
    }
}

#[derive(Clone, Debug, Default)]
pub struct HostSample {
    pub keys: KeySet,
    pub mouse_buttons: u8,
    pub pointer: Option<[i16; 2]>,
    pub delta: [f32; 2],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    Button(u8),
    Key(KeyCode),
    MouseButton(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PointerAxis {
    X,
    Y,
}

impl PointerAxis {
    const fn index(self) -> usize {
        match self {
            Self::X => 0,
            Self::Y => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AxisInput {
    Axis(u8),
    Button(u8),
    KeyPair(KeyCode, KeyCode),
    MouseDelta(PointerAxis),
    MousePosition(PointerAxis),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AxisSource {
    pub source: AxisInput,
    pub invert: bool,
    pub scale: f32,
    pub dead_zone: f32,
}

impl AxisSource {
    const fn axis(source: u8, invert: bool) -> Self {
        Self {
            source: AxisInput::Axis(source),
            invert,
            scale: 1.0,
            dead_zone: 0.0,
        }
    }

    const fn button(source: u8) -> Self {
        Self {
            source: AxisInput::Button(source),
            invert: false,
            scale: 1.0,
            dead_zone: 0.0,
        }
    }

    fn read(self, gamepad: &GamepadReport, host: &HostSample) -> i16 {
        let value = match self.source {
            AxisInput::Axis(index) => Self::gamepad_axis(gamepad, index),
            AxisInput::Button(index) => {
                if gamepad.buttons & (1 << index) != 0 {
                    i16::MAX
                } else {
                    0
                }
            }
            AxisInput::KeyPair(negative, positive) => {
                match (host.keys.contains(negative), host.keys.contains(positive)) {
                    (true, false) => i16::MIN,
                    (false, true) => i16::MAX,
                    _ => 0,
                }
            }
            AxisInput::MouseDelta(axis) => {
                (host.delta[axis.index()].clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
            }
            AxisInput::MousePosition(axis) => host.pointer.map_or(0, |value| value[axis.index()]),
        };
        let value = if self.invert {
            value.saturating_neg()
        } else {
            value
        };
        let threshold = (self.dead_zone.clamp(0.0, 1.0) * f32::from(i16::MAX)) as i32;
        if i32::from(value).abs() <= threshold {
            return 0;
        }
        (f32::from(value) * self.scale).clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
    }

    fn gamepad_axis(gamepad: &GamepadReport, index: u8) -> i16 {
        match index {
            0 => gamepad.left_stick[0],
            1 => gamepad.left_stick[1],
            2 => gamepad.right_stick[0],
            3 => gamepad.right_stick[1],
            4 => (u32::from(gamepad.left_trigger) * i16::MAX as u32 / u16::MAX as u32) as i16,
            5 => (u32::from(gamepad.right_trigger) * i16::MAX as u32 / u16::MAX as u32) as i16,
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Target {
    Button(u8),
    Axis { index: u8, direction: i8 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MappingError(String);

impl MappingError {
    fn invalid(target: BindingKey, source: BindingKey) -> Self {
        Self(format!("cannot map input source {source} to {target}"))
    }
}

impl fmt::Display for MappingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for MappingError {}

#[derive(Clone)]
pub struct Mapping {
    buttons: [Option<Source>; 21],
    axes: [Option<AxisSource>; 8],
    keys: Vec<(KeyCode, Target)>,
}

impl Mapping {
    pub fn compile(
        layout: &InputLayout,
        mode: crate::layout::ProjectionMode,
        bindings: &[(BindingKey, Option<BindingKey>)],
    ) -> Result<Self, MappingError> {
        let mut selected = HashMap::new();
        if mode != crate::layout::ProjectionMode::Custom {
            selected.extend(
                layout
                    .default_gamepad_bindings()
                    .map(|(target, source)| (target, Some(source))),
            );
        }
        selected.extend(bindings.iter().copied());
        let mut mapping = Self {
            buttons: [None; 21],
            axes: [None; 8],
            keys: Vec::new(),
        };
        for (target, source) in selected {
            mapping.bind_gamepad(layout, target, source)?;
        }
        if mode != crate::layout::ProjectionMode::Custom {
            mapping.bind_keyboard(layout);
        }
        Ok(mapping)
    }

    pub fn identity(layout: &InputLayout, mode: crate::layout::ProjectionMode) -> Self {
        Self::compile(layout, mode, &[]).expect("an input layout's embedded mapping is valid")
    }

    pub fn apply(&self, source: &GamepadReport, host: &HostSample) -> GamepadReport {
        let mut output = GamepadReport::default();
        for (target, binding) in self.buttons.iter().enumerate() {
            if binding.is_some_and(|binding| Self::pressed(binding, source, host)) {
                output.buttons |= 1 << target;
            }
        }
        for (target, binding) in self.axes.iter().enumerate() {
            if let Some(binding) = binding {
                Self::set_axis(&mut output, target, binding.read(source, host));
            }
        }
        for (code, target) in &self.keys {
            if host.keys.contains(*code) {
                Self::apply_key(&mut output, *target);
            }
        }
        output
    }

    pub fn consumes_key(&self, code: KeyCode) -> bool {
        self.keys.iter().any(|(candidate, _)| *candidate == code)
            || self.axes.iter().flatten().any(|axis| {
                matches!(axis.source, AxisInput::KeyPair(negative, positive) if negative == code || positive == code)
            })
            || self
                .buttons
                .iter()
                .flatten()
                .any(|source| matches!(source, Source::Key(candidate) if *candidate == code))
    }

    fn bind_gamepad(
        &mut self,
        layout: &InputLayout,
        target: BindingKey,
        source: Option<BindingKey>,
    ) -> Result<(), MappingError> {
        let Some(target_wire) = layout.sdl_wire(target) else {
            return Ok(());
        };
        let Some(source) = source else {
            self.clear(target_wire);
            return Ok(());
        };
        match (target_wire, layout.custom_binding(target, source)) {
            (Wire::Button(target), Some(InputBinding::Button { button })) => {
                self.buttons[usize::from(target.index())] = Some(Source::Button(button.index()));
            }
            (Wire::Axis(target), Some(InputBinding::Axis { axis, invert })) => {
                self.axes[usize::from(target.index())] =
                    Some(AxisSource::axis(axis.index(), invert));
            }
            (Wire::Axis(target), Some(InputBinding::Button { button })) => {
                self.axes[usize::from(target.index())] = Some(AxisSource::button(button.index()));
            }
            _ => return Err(MappingError::invalid(target, source)),
        }
        Ok(())
    }

    fn clear(&mut self, target: Wire) {
        match target {
            Wire::Button(button) => self.buttons[usize::from(button.index())] = None,
            Wire::Axis(axis) => self.axes[usize::from(axis.index())] = None,
            Wire::RawAxis(index) if usize::from(index) < self.axes.len() => {
                self.axes[usize::from(index)] = None;
            }
            Wire::RawAxis(_) | Wire::Sensor(_) => {}
        }
    }

    fn bind_keyboard(&mut self, layout: &InputLayout) {
        let Ok(mapping) = KeyboardMapping::from_toml(EMBEDDED_DEFAULT_KEYBOARD_MAPPING) else {
            return;
        };
        for (canonical, code) in mapping.bindings {
            let Some(target) = layout
                .resolve(canonical)
                .and_then(|key| Self::target(layout, key))
            else {
                continue;
            };
            self.keys.push((KeyCode::new(&code), target));
        }
    }

    fn target(layout: &InputLayout, key: BindingKey) -> Option<Target> {
        match layout.sdl_wire(key)? {
            Wire::Button(button) => Some(Target::Button(button.index())),
            Wire::Axis(axis) => {
                let positive = !matches!(
                    key,
                    BindingKey::Analog {
                        component: Component {
                            sign: Sign::Negative,
                            ..
                        },
                        ..
                    }
                );
                let direction = match (axis.index() % 2, positive) {
                    (1, true) => -1,
                    (1, false) => 1,
                    (_, true) => 1,
                    (_, false) => -1,
                };
                Some(Target::Axis {
                    index: axis.index(),
                    direction,
                })
            }
            Wire::RawAxis(_) | Wire::Sensor(_) => None,
        }
    }

    fn pressed(source: Source, gamepad: &GamepadReport, host: &HostSample) -> bool {
        match source {
            Source::Button(index) => gamepad.buttons & (1 << index) != 0,
            Source::Key(code) => host.keys.contains(code),
            Source::MouseButton(index) => host.mouse_buttons & (1 << index) != 0,
        }
    }

    fn set_axis(output: &mut GamepadReport, index: usize, value: i16) {
        match index {
            0 => output.left_stick[0] = value,
            1 => output.left_stick[1] = value,
            2 => output.right_stick[0] = value,
            3 => output.right_stick[1] = value,
            4 => output.left_trigger = Self::trigger(value),
            5 => output.right_trigger = Self::trigger(value),
            _ => {}
        }
    }

    fn trigger(value: i16) -> u16 {
        let value = u32::from(value.max(0) as u16);
        ((value * u32::from(u16::MAX)) / i16::MAX as u32) as u16
    }

    fn apply_key(output: &mut GamepadReport, target: Target) {
        match target {
            Target::Button(index) => output.buttons |= 1 << index,
            Target::Axis { index, direction } if index < 4 => {
                let value = if direction < 0 { i16::MIN } else { i16::MAX };
                let current = match index {
                    0 => output.left_stick[0],
                    1 => output.left_stick[1],
                    2 => output.right_stick[0],
                    _ => output.right_stick[1],
                };
                if current == 0 || value.signum() == current.signum() {
                    Self::set_axis(output, usize::from(index), value);
                } else {
                    Self::set_axis(output, usize::from(index), 0);
                }
            }
            Target::Axis { index, .. } => Self::set_axis(output, usize::from(index), i16::MAX),
        }
    }
}
