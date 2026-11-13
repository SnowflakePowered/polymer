use consolespec::input::{BindingKey, Component, Sign};
use polymer_config::input::{
    ControllerBindings, InputSource, Orientation, PointerAxis, ProjectionMode,
};
use polymer_input::{GamepadButton, GamepadReport, KeyCode};
use std::collections::BTreeSet;
use std::fmt;

use crate::layout::{
    EMBEDDED_DEFAULT_KEYBOARD_MAPPING, InputBinding, InputLayout, KeyboardMapping,
};
use crate::model::{self, Control};

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
    MouseButton(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AxisInput {
    Axis(u8),
    Button(u8),
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

    const fn pointer(source: AxisInput, invert: bool) -> Self {
        Self {
            source,
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
            AxisInput::Button(index) => GamepadButton::from_wire(index).map_or(0, |button| {
                (i32::from(gamepad.button_pressure(button)) * i32::from(i16::MAX) / 255) as i16
            }),
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
    fn invalid(target: BindingKey, source: impl fmt::Display) -> Self {
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
        mode: ProjectionMode,
        orientation: Orientation,
        bindings: &ControllerBindings,
    ) -> Result<Self, MappingError> {
        let mut mapping = Self {
            buttons: [None; 21],
            axes: [None; 8],
            keys: Vec::new(),
        };
        for (target, source) in layout.resolved_bindings(mode, orientation, bindings).iter() {
            mapping.bind(layout, *target, source)?;
        }
        if mode != ProjectionMode::Custom {
            mapping.bind_keyboard(layout, bindings);
        }
        Ok(mapping)
    }

    pub fn identity(layout: &InputLayout, mode: ProjectionMode) -> Self {
        Self::compile(
            layout,
            mode,
            Orientation::Vertical,
            &ControllerBindings::default(),
        )
        .expect("an input layout's embedded mapping is valid")
    }

    pub fn apply(&self, source: &GamepadReport, host: &HostSample) -> GamepadReport {
        let mut output = GamepadReport::default();
        for (target, binding) in self.buttons.iter().enumerate() {
            if let Some(Source::Button(index)) = binding
                && source.pressure.mask & (1 << index) != 0
            {
                output.pressure.mask |= 1 << target;
                output.pressure.values[target] = source.pressure.values[usize::from(*index)];
            }
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
    }

    fn bind(
        &mut self,
        layout: &InputLayout,
        target_key: BindingKey,
        source: Option<&InputSource>,
    ) -> Result<(), MappingError> {
        let Some(target_control) = layout.projected_control(target_key) else {
            return Ok(());
        };
        let Some(source) = source else {
            self.clear(target_control);
            return Ok(());
        };
        let invalid = || MappingError::invalid(target_key, source);
        match (source, target_control) {
            (InputSource::Pad(key), _) => {
                self.bind_gamepad(layout, target_key, target_control, *key)
            }
            (InputSource::Key(code), _) => {
                let target = Self::target(layout, target_key).ok_or_else(invalid)?;
                self.keys.push((KeyCode::new(code), target));
                Ok(())
            }
            (InputSource::MouseButton(index), Control::Button(target)) => {
                self.buttons[usize::from(target)] = Some(Source::MouseButton(*index));
                Ok(())
            }
            (InputSource::MouseDelta(axis), Control::Axis(target)) => {
                self.axes[usize::from(target)] = Some(AxisSource::pointer(
                    AxisInput::MouseDelta(*axis),
                    layout.axis_inverted(target_key),
                ));
                Ok(())
            }
            (InputSource::MousePosition(axis), Control::Axis(target)) => {
                self.axes[usize::from(target)] = Some(AxisSource::pointer(
                    AxisInput::MousePosition(*axis),
                    layout.axis_inverted(target_key),
                ));
                Ok(())
            }
            _ => Err(invalid()),
        }
    }

    fn bind_gamepad(
        &mut self,
        layout: &InputLayout,
        target_key: BindingKey,
        target_control: Control,
        source: BindingKey,
    ) -> Result<(), MappingError> {
        match (target_control, layout.custom_binding(target_key, source)) {
            (Control::Button(target), Some(InputBinding::Button { button })) => {
                let Some(Control::Button(source)) = Control::from_key(button) else {
                    return Err(MappingError::invalid(target_key, source));
                };
                self.buttons[usize::from(target)] = Some(Source::Button(source));
            }
            (Control::Axis(target), Some(InputBinding::Axis { axis, invert })) => {
                let Some(Control::Axis(source)) = Control::from_key(axis) else {
                    return Err(MappingError::invalid(target_key, source));
                };
                self.axes[usize::from(target)] = Some(AxisSource::axis(source, invert));
            }
            (Control::Axis(target), Some(InputBinding::Button { button })) => {
                let Some(Control::Button(source)) = Control::from_key(button) else {
                    return Err(MappingError::invalid(target_key, source));
                };
                self.axes[usize::from(target)] = Some(AxisSource::button(source));
            }
            _ => return Err(MappingError::invalid(target_key, source)),
        }
        Ok(())
    }

    fn clear(&mut self, target: Control) {
        match target {
            Control::Button(index) => self.buttons[usize::from(index)] = None,
            Control::Axis(index) if usize::from(index) < self.axes.len() => {
                self.axes[usize::from(index)] = None;
            }
            Control::Axis(_) | Control::Sensor(_) => {}
        }
    }

    fn bind_keyboard(&mut self, layout: &InputLayout, overrides: &ControllerBindings) {
        let Ok(mapping) = KeyboardMapping::from_toml(EMBEDDED_DEFAULT_KEYBOARD_MAPPING) else {
            return;
        };
        for (canonical, code) in mapping.bindings {
            let Some(key) = layout
                .resolve(canonical)
                .filter(|key| !overrides.contains(*key))
            else {
                continue;
            };
            if let Some(target) = Self::target(layout, key) {
                self.keys.push((KeyCode::new(&code), target));
            }
        }
    }

    fn target(layout: &InputLayout, key: BindingKey) -> Option<Target> {
        match layout.projected_control(key)? {
            Control::Button(index) => Some(Target::Button(index)),
            Control::Axis(index) if usize::from(index) < model::StandardGamepad::AXES.len() => {
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
                let direction = match (index % 2, positive) {
                    (1, true) => -1,
                    (1, false) => 1,
                    (_, true) => 1,
                    (_, false) => -1,
                };
                Some(Target::Axis { index, direction })
            }
            Control::Axis(_) | Control::Sensor(_) => None,
        }
    }

    fn pressed(source: Source, gamepad: &GamepadReport, host: &HostSample) -> bool {
        match source {
            Source::Button(index) => gamepad.buttons & (1 << index) != 0,

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
            Target::Button(index) => {
                output.buttons |= 1 << index;
                output.pressure.values[usize::from(index)] = u8::MAX;
            }
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
