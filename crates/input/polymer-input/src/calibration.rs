use crate::DeviceState;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StickCalibration {
    pub center: [i16; 2],
    pub min: [i16; 2],
    pub max: [i16; 2],
    pub dead_zone: f32,
    pub outer_dead_zone: f32,
}

impl Default for StickCalibration {
    fn default() -> Self {
        Self {
            center: [0; 2],
            min: [i16::MIN; 2],
            max: [i16::MAX; 2],
            dead_zone: 0.0,
            outer_dead_zone: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TriggerCalibration {
    pub min: u16,
    pub max: u16,
    pub dead_zone: f32,
}

impl Default for TriggerCalibration {
    fn default() -> Self {
        Self {
            min: 0,
            max: u16::MAX,
            dead_zone: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionCalibration {
    pub accelerometer_bias: [f32; 3],
    pub accelerometer_scale: [f32; 3],
    pub gyroscope_bias: [f32; 3],
    pub gyroscope_scale: [f32; 3],
}

impl Default for MotionCalibration {
    fn default() -> Self {
        Self {
            accelerometer_bias: [0.0; 3],
            accelerometer_scale: [1.0; 3],
            gyroscope_bias: [0.0; 3],
            gyroscope_scale: [1.0; 3],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SensorBarPosition {
    #[default]
    Above,
    Below,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IrSensitivity {
    Level1,
    Level2,
    #[default]
    Level3,
    Level4,
    Level5,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IrMode {
    #[default]
    Basic,
    Extended,
    Full,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PointerSetup {
    pub sensor_bar: SensorBarPosition,
    pub sensitivity: IrSensitivity,
    pub mode: IrMode,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Calibration {
    pub sticks: [Option<StickCalibration>; 2],
    pub triggers: [Option<TriggerCalibration>; 2],
    pub motion: Option<MotionCalibration>,
    pub pointer: Option<PointerSetup>,
}

impl Calibration {
    pub fn merge(&mut self, update: &Self) {
        for index in 0..2 {
            if update.sticks[index].is_some() {
                self.sticks[index] = update.sticks[index];
            }
            if update.triggers[index].is_some() {
                self.triggers[index] = update.triggers[index];
            }
        }
        if update.motion.is_some() {
            self.motion = update.motion;
        }
        if update.pointer.is_some() {
            self.pointer = update.pointer;
        }
    }

    pub fn without_shaping(&self) -> Self {
        let mut calibration = self.clone();
        for stick in calibration.sticks.iter_mut().flatten() {
            stick.dead_zone = 0.0;
            stick.outer_dead_zone = 0.0;
        }
        for trigger in calibration.triggers.iter_mut().flatten() {
            trigger.dead_zone = 0.0;
        }
        calibration
    }

    pub fn correct(&self, state: &mut DeviceState) {
        let sticks = [
            &mut state.gamepad.left_stick,
            &mut state.gamepad.right_stick,
        ];
        for (values, calibration) in sticks
            .into_iter()
            .zip(self.sticks)
            .filter_map(|(v, c)| c.map(|c| (v, c)))
        {
            for axis in 0..2 {
                let centered = i32::from(values[axis]) - i32::from(calibration.center[axis]);
                let range = if centered < 0 {
                    i32::from(calibration.center[axis]) - i32::from(calibration.min[axis])
                } else {
                    i32::from(calibration.max[axis]) - i32::from(calibration.center[axis])
                }
                .max(1);
                values[axis] = (centered * i32::from(i16::MAX) / range)
                    .clamp(i32::from(i16::MIN), i32::from(i16::MAX))
                    as i16;
            }
        }
        let triggers = [
            &mut state.gamepad.left_trigger,
            &mut state.gamepad.right_trigger,
        ];
        for (value, calibration) in triggers
            .into_iter()
            .zip(self.triggers)
            .filter_map(|(v, c)| c.map(|c| (v, c)))
        {
            let range = calibration.max.saturating_sub(calibration.min).max(1);
            *value = value
                .saturating_sub(calibration.min)
                .saturating_mul(u16::MAX)
                / range;
        }
        if let Some(motion) = self.motion {
            for axis in 0..3 {
                state.sensor.accelerometer[axis] = (state.sensor.accelerometer[axis]
                    - motion.accelerometer_bias[axis])
                    * motion.accelerometer_scale[axis];
                state.sensor.gyroscope[axis] = (state.sensor.gyroscope[axis]
                    - motion.gyroscope_bias[axis])
                    * motion.gyroscope_scale[axis];
            }
        }
    }

    pub fn shape(&self, state: &mut DeviceState) {
        let sticks = [
            &mut state.gamepad.left_stick,
            &mut state.gamepad.right_stick,
        ];
        for (values, calibration) in sticks
            .into_iter()
            .zip(self.sticks)
            .filter_map(|(v, c)| c.map(|c| (v, c)))
        {
            let x = f32::from(values[0]) / f32::from(i16::MAX);
            let y = f32::from(values[1]) / f32::from(i16::MAX);
            let magnitude = (x * x + y * y).sqrt();
            let outer = (1.0 - calibration.outer_dead_zone).max(calibration.dead_zone);
            if magnitude <= calibration.dead_zone {
                *values = [0; 2];
            } else {
                let shaped = ((magnitude - calibration.dead_zone)
                    / (outer - calibration.dead_zone).max(f32::EPSILON))
                .clamp(0.0, 1.0);
                let scale = shaped / magnitude.max(f32::EPSILON);
                values[0] = (x * scale * f32::from(i16::MAX)) as i16;
                values[1] = (y * scale * f32::from(i16::MAX)) as i16;
            }
        }
        let triggers = [
            &mut state.gamepad.left_trigger,
            &mut state.gamepad.right_trigger,
        ];
        for (value, calibration) in triggers
            .into_iter()
            .zip(self.triggers)
            .filter_map(|(v, c)| c.map(|c| (v, c)))
        {
            let normalized = f32::from(*value) / f32::from(u16::MAX);
            *value = if normalized <= calibration.dead_zone {
                0
            } else {
                (((normalized - calibration.dead_zone) / (1.0 - calibration.dead_zone))
                    * f32::from(u16::MAX)) as u16
            };
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CalibrationKind {
    StickCenter,
    StickRange,
    TriggerRange,
    MotionBias,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CalibrationError {
    Cancelled,
    InsufficientSamples,
    DeviceUnavailable,
}

impl std::fmt::Display for CalibrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for CalibrationError {}
