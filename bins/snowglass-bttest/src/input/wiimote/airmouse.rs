//! Accelerometer-assisted MotionPlus pointer integration.

use std::time::Instant;

use super::{Accelerometer, Gyroscope};

const RAW_GYRO_CENTER: f32 = 8192.0;
// Nominal MotionPlus scales derived from its default calibration. Actual
// hardware calibration can replace these constants when EEPROM reads land.
const SLOW_RAW_PER_RADIAN_PER_SECOND: f32 = 923.5;
const FAST_RAW_PER_RADIAN_PER_SECOND: f32 = 207.8;
const POINTER_GAIN: f32 = 0.78;
const MAX_TIMESTEP_SECONDS: f32 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerPosition {
    pub x: f32,
    pub y: f32,
}

impl PointerPosition {
    pub const CENTER: Self = Self { x: 0.5, y: 0.5 };

    pub fn clamped(self) -> Self {
        Self {
            x: self.x.clamp(0.0, 1.0),
            y: self.y.clamp(0.0, 1.0),
        }
    }
}

/// Stateful gyro integrator with accelerometer-assisted bias correction.
///
/// Yaw and pitch are integrated without pulling the pointer back toward the
/// center. The accelerometer detects stationary periods so the gyro zero can
/// be continuously refined to suppress cursor creep.
#[derive(Debug)]
pub struct AirMouse {
    position: PointerPosition,
    horizontal_angle: f32,
    vertical_angle: f32,
    gyro_bias: [f32; 3],
    gravity_reference_pitch: Option<f32>,
    last_accelerometer: Option<Accelerometer>,
    last_update: Option<Instant>,
}

impl Default for AirMouse {
    fn default() -> Self {
        Self {
            position: PointerPosition::CENTER,
            horizontal_angle: 0.0,
            vertical_angle: 0.0,
            gyro_bias: [RAW_GYRO_CENTER; 3],
            gravity_reference_pitch: None,
            last_accelerometer: None,
            last_update: None,
        }
    }
}

impl AirMouse {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn position(&self) -> PointerPosition {
        self.position
    }

    /// Advances the fused pointer by one timestamped sensor sample.
    pub fn next_pointer_position(
        &mut self,
        accelerometer: Accelerometer,
        gyroscope: Gyroscope,
        timestamp: Instant,
    ) -> PointerPosition {
        self.last_accelerometer = Some(accelerometer);
        let accel_pitch = accelerometer_pitch(accelerometer);
        self.gravity_reference_pitch.get_or_insert(accel_pitch);

        let dt = self.last_update.replace(timestamp).map_or(0.0, |previous| {
            timestamp
                .saturating_duration_since(previous)
                .as_secs_f32()
                .min(MAX_TIMESTEP_SECONDS)
        });

        if dt > 0.0 {
            let pitch_rate = raw_rate(gyroscope.pitch, self.gyro_bias[0], gyroscope.pitch_slow);
            let roll_rate = raw_rate(gyroscope.roll, self.gyro_bias[1], gyroscope.roll_slow);
            let yaw_rate = raw_rate(gyroscope.yaw, self.gyro_bias[2], gyroscope.yaw_slow);

            // MotionPlus yaw increases opposite to screen-space X.
            self.horizontal_angle -= yaw_rate * dt;
            self.vertical_angle += pitch_rate * dt;

            self.update_stationary_bias(
                accelerometer,
                gyroscope,
                [pitch_rate, roll_rate, yaw_rate],
            );
            self.update_position();
        }

        self.position
    }

    /// Centers the pointer and treats the current resting angle as level.
    /// This intentionally leaves gyro bias untouched.
    pub fn center(&mut self) {
        self.position = PointerPosition::CENTER;
        self.horizontal_angle = 0.0;
        self.vertical_angle = 0.0;
        if let Some(accelerometer) = self.last_accelerometer {
            self.gravity_reference_pitch = Some(accelerometer_pitch(accelerometer));
        }
    }

    /// Relearns the gyro zero and gravity reference from a stationary sample,
    /// then centers the pointer.
    pub fn recalibrate(&mut self, accelerometer: Accelerometer, gyroscope: Gyroscope) {
        self.gyro_bias = [
            f32::from(gyroscope.pitch),
            f32::from(gyroscope.roll),
            f32::from(gyroscope.yaw),
        ];
        self.last_accelerometer = Some(accelerometer);
        self.gravity_reference_pitch = Some(accelerometer_pitch(accelerometer));
        self.last_update = None;
        self.center();
    }

    /// Synchronizes the fallback integrator to an absolute pointer source.
    pub fn set_position(&mut self, position: PointerPosition) {
        self.position = position.clamped();
        self.horizontal_angle = (self.position.x - 0.5) / POINTER_GAIN;
        self.vertical_angle = (self.position.y - 0.5) / POINTER_GAIN;
    }

    fn update_position(&mut self) {
        self.position = PointerPosition {
            x: 0.5 + self.horizontal_angle * POINTER_GAIN,
            y: 0.5 + self.vertical_angle * POINTER_GAIN,
        }
        .clamped();

        // Avoid accumulating invisible angle beyond the screen boundary.
        self.horizontal_angle = (self.position.x - 0.5) / POINTER_GAIN;
        self.vertical_angle = (self.position.y - 0.5) / POINTER_GAIN;
    }

    fn update_stationary_bias(
        &mut self,
        accelerometer: Accelerometer,
        gyroscope: Gyroscope,
        rates: [f32; 3],
    ) {
        let Some(reference_pitch) = self.gravity_reference_pitch else {
            return;
        };
        let gravity_is_steady =
            wrap_angle(accelerometer_pitch(accelerometer) - reference_pitch).abs() < 0.025;
        let gyro_is_steady = rates.into_iter().all(|rate| rate.abs() < 0.035);
        if gravity_is_steady && gyro_is_steady {
            const BIAS_BLEND: f32 = 0.002;
            for (bias, sample) in
                self.gyro_bias
                    .iter_mut()
                    .zip([gyroscope.pitch, gyroscope.roll, gyroscope.yaw])
            {
                *bias += (f32::from(sample) - *bias) * BIAS_BLEND;
            }
        }
    }
}

fn raw_rate(raw: u16, bias: f32, slow: bool) -> f32 {
    (f32::from(raw) - bias)
        / if slow {
            SLOW_RAW_PER_RADIAN_PER_SECOND
        } else {
            FAST_RAW_PER_RADIAN_PER_SECOND
        }
}

fn accelerometer_pitch(accelerometer: Accelerometer) -> f32 {
    let x = f32::from(accelerometer.x) - 512.0;
    let y = f32::from(accelerometer.y) - 512.0;
    let z = f32::from(accelerometer.z) - 512.0;
    (-x).atan2((y * y + z * z).sqrt())
}

fn wrap_angle(angle: f32) -> f32 {
    let two_pi = std::f32::consts::TAU;
    (angle + std::f32::consts::PI).rem_euclid(two_pi) - std::f32::consts::PI
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn level_accel() -> Accelerometer {
        Accelerometer {
            x: 512,
            y: 512,
            z: 616,
        }
    }

    fn gyro(pitch: u16, yaw: u16) -> Gyroscope {
        Gyroscope {
            pitch,
            roll: 8192,
            yaw,
            pitch_slow: true,
            roll_slow: true,
            yaw_slow: true,
        }
    }

    #[test]
    fn integrates_yaw_and_recenters() {
        let start = Instant::now();
        let mut air_mouse = AirMouse::new();
        air_mouse.recalibrate(level_accel(), gyro(8192, 8192));
        air_mouse.next_pointer_position(level_accel(), gyro(8192, 8192), start);
        let moved = air_mouse.next_pointer_position(
            level_accel(),
            gyro(8192, 9116),
            start + Duration::from_millis(50),
        );
        assert!(moved.x < 0.47);
        air_mouse.center();
        assert_eq!(air_mouse.position(), PointerPosition::CENTER);
    }

    #[test]
    fn recalibration_removes_constant_gyro_offset() {
        let start = Instant::now();
        let mut air_mouse = AirMouse::new();
        air_mouse.recalibrate(level_accel(), gyro(8250, 8240));
        air_mouse.next_pointer_position(level_accel(), gyro(8250, 8240), start);
        let position = air_mouse.next_pointer_position(
            level_accel(),
            gyro(8250, 8240),
            start + Duration::from_millis(50),
        );
        assert_eq!(position, PointerPosition::CENTER);
    }

    #[test]
    fn vertical_position_does_not_return_to_center_while_still() {
        let start = Instant::now();
        let mut air_mouse = AirMouse::new();
        air_mouse.recalibrate(level_accel(), gyro(8192, 8192));
        air_mouse.next_pointer_position(level_accel(), gyro(8192, 8192), start);
        let moved = air_mouse.next_pointer_position(
            level_accel(),
            gyro(9116, 8192),
            start + Duration::from_millis(50),
        );
        assert!(moved.y > 0.53);

        let resting = air_mouse.next_pointer_position(
            level_accel(),
            gyro(8192, 8192),
            start + Duration::from_secs(1),
        );
        assert!((resting.y - moved.y).abs() < f32::EPSILON);
    }

    #[test]
    fn absolute_position_is_clamped_and_adopted() {
        let mut air_mouse = AirMouse::new();
        air_mouse.set_position(PointerPosition { x: 1.2, y: -0.1 });
        assert_eq!(air_mouse.position(), PointerPosition { x: 1.0, y: 0.0 });
    }
}
