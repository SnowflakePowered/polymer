use crate::decoder::MotionFrame;
use std::time::{Duration, Instant};

const GRAVITY: f32 = 9.80665;
const MAX_STATIONARY_ACCEL_ERROR: f32 = 0.45;
const MAX_STATIONARY_ANGULAR_RATE: f32 = 0.035;
const STATIONARY_CONFIRMATION: Duration = Duration::from_millis(500);
const DRIFT_ESTIMATE_ALPHA: f32 = 0.02;

#[derive(Clone, Copy)]
struct Quaternion {
    w: f32,
    x: f32,
    y: f32,
    z: f32,
}

impl Quaternion {
    fn from_array([w, x, y, z]: [f32; 4]) -> Self {
        Self { w, x, y, z }.normalized()
    }

    fn from_rotation_vector([x, y, z]: [f32; 3]) -> Self {
        let angle = (x * x + y * y + z * z).sqrt();
        if angle <= f32::EPSILON {
            return Self {
                w: 1.0,
                x: 0.0,
                y: 0.0,
                z: 0.0,
            };
        }
        let scale = (angle * 0.5).sin() / angle;
        Self {
            w: (angle * 0.5).cos(),
            x: x * scale,
            y: y * scale,
            z: z * scale,
        }
    }

    fn normalized(self) -> Self {
        let norm = (self.w * self.w + self.x * self.x + self.y * self.y + self.z * self.z).sqrt();
        Self {
            w: self.w / norm,
            x: self.x / norm,
            y: self.y / norm,
            z: self.z / norm,
        }
    }

    fn conjugate(self) -> Self {
        Self {
            w: self.w,
            x: -self.x,
            y: -self.y,
            z: -self.z,
        }
    }

    fn multiplied(self, rhs: Self) -> Self {
        Self {
            w: self.w * rhs.w - self.x * rhs.x - self.y * rhs.y - self.z * rhs.z,
            x: self.w * rhs.x + self.x * rhs.w + self.y * rhs.z - self.z * rhs.y,
            y: self.w * rhs.y - self.x * rhs.z + self.y * rhs.w + self.z * rhs.x,
            z: self.w * rhs.z + self.x * rhs.y - self.y * rhs.x + self.z * rhs.w,
        }
        .normalized()
    }

    fn rotation_vector(self) -> [f32; 3] {
        let orientation = if self.w < 0.0 {
            Self {
                w: -self.w,
                x: -self.x,
                y: -self.y,
                z: -self.z,
            }
        } else {
            self
        };
        let sine = (orientation.x * orientation.x
            + orientation.y * orientation.y
            + orientation.z * orientation.z)
            .sqrt();
        if sine <= f32::EPSILON {
            return [0.0; 3];
        }
        let scale = 2.0 * sine.atan2(orientation.w) / sine;
        [
            orientation.x * scale,
            orientation.y * scale,
            orientation.z * scale,
        ]
    }

    fn to_array(self) -> [f32; 4] {
        [self.w, self.x, self.y, self.z]
    }
}

#[derive(Default)]
pub(super) struct OrientationTracker {
    previous_raw: Option<Quaternion>,
    previous_sampled_at: Option<Instant>,
    stabilized: Option<Quaternion>,
    stationary_since: Option<Instant>,
    stationary_anchor: Option<Quaternion>,
    drift_rate: [f32; 3],
    drift_estimate_ready: bool,
}

impl OrientationTracker {
    pub fn update(&mut self, motion: MotionFrame) -> [f32; 4] {
        let raw = Quaternion::from_array(motion.orientation);
        let (Some(previous_raw), Some(previous_sampled_at), Some(stabilized)) =
            (self.previous_raw, self.previous_sampled_at, self.stabilized)
        else {
            self.previous_raw = Some(raw);
            self.previous_sampled_at = Some(motion.orientation_sampled_at);
            self.stabilized = Some(raw);
            return raw.to_array();
        };
        let elapsed = motion
            .orientation_sampled_at
            .saturating_duration_since(previous_sampled_at)
            .as_secs_f32()
            .clamp(0.001, 0.100);
        let raw_delta = previous_raw.conjugate().multiplied(raw);
        let rotation_rate = raw_delta.rotation_vector().map(|value| value / elapsed);
        let stationary = Self::is_stationary(motion, rotation_rate);

        self.previous_raw = Some(raw);
        self.previous_sampled_at = Some(motion.orientation_sampled_at);
        if stationary {
            let stationary_since = *self.stationary_since.get_or_insert_with(|| {
                self.stationary_anchor = Some(stabilized);
                motion.orientation_sampled_at
            });
            if motion
                .orientation_sampled_at
                .saturating_duration_since(stationary_since)
                >= STATIONARY_CONFIRMATION
            {
                self.update_drift_estimate(rotation_rate);
                let anchored = self.stationary_anchor.unwrap_or(stabilized);
                self.stabilized = Some(anchored);
                return anchored.to_array();
            }
        } else {
            self.stationary_since = None;
            self.stationary_anchor = None;
        }

        let corrected_delta = if self.drift_estimate_ready {
            let drift_delta =
                Quaternion::from_rotation_vector(self.drift_rate.map(|rate| rate * elapsed));
            raw_delta.multiplied(drift_delta.conjugate())
        } else {
            raw_delta
        };
        let stabilized = stabilized.multiplied(corrected_delta);
        self.stabilized = Some(stabilized);
        stabilized.to_array()
    }

    fn is_stationary(motion: MotionFrame, rotation_rate: [f32; 3]) -> bool {
        let acceleration = motion
            .filtered_accelerometer
            .unwrap_or(motion.accelerometer[2].value);
        let acceleration_magnitude = acceleration
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        let gyroscope = motion.gyroscope[1].value;
        let gyroscope_magnitude = gyroscope
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        let orientation_rate = rotation_rate
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt();
        (acceleration_magnitude - GRAVITY).abs() <= MAX_STATIONARY_ACCEL_ERROR
            && gyroscope_magnitude <= MAX_STATIONARY_ANGULAR_RATE
            && orientation_rate <= MAX_STATIONARY_ANGULAR_RATE
    }

    fn update_drift_estimate(&mut self, measured_rate: [f32; 3]) {
        if self.drift_estimate_ready {
            for (estimate, measured) in self.drift_rate.iter_mut().zip(measured_rate) {
                *estimate += (measured - *estimate) * DRIFT_ESTIMATE_ALPHA;
            }
        } else {
            self.drift_rate = measured_rate;
            self.drift_estimate_ready = true;
        }
    }
}
