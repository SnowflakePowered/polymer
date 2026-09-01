use super::{HubError, InputHub};
use crate::{
    Calibration, CalibrationError, CalibrationKind, DeviceState, Feedback, TriggerCalibration,
};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
pub(crate) struct CalibrationSession {
    calibration: Calibration,
    capture: Option<CaptureState>,
}

impl CalibrationSession {
    pub(crate) fn calibration(&self) -> Calibration {
        self.calibration.clone()
    }

    pub(crate) fn merge(&mut self, update: &Calibration) -> Feedback {
        self.calibration.merge(update);
        self.feedback()
    }

    pub(crate) fn replace(&mut self, calibration: Calibration) -> Feedback {
        self.calibration = calibration;
        self.feedback()
    }

    pub(crate) fn begin(&mut self, kind: CalibrationKind) -> Result<Feedback, CalibrationError> {
        if self.capture.is_some() {
            return Err(CalibrationError::Cancelled);
        }
        self.capture = Some(CaptureState::new(kind));
        Ok(Feedback::Calibrate(self.calibration.without_shaping()))
    }

    pub(crate) fn accept(&mut self, state: &DeviceState) {
        if let Some(capture) = &mut self.capture {
            capture.accept(state);
        }
    }

    pub(crate) fn progress(&self) -> f32 {
        self.capture.as_ref().map_or(0.0, CaptureState::progress)
    }

    pub(crate) fn finish(&mut self) -> Result<Feedback, CalibrationError> {
        let capture = self.capture.take().ok_or(CalibrationError::Cancelled)?;
        self.calibration = capture.finish(&self.calibration)?;
        Ok(self.feedback())
    }

    pub(crate) fn cancel(&mut self) -> Option<Feedback> {
        self.capture.take().map(|_| self.feedback())
    }

    pub(crate) fn feedback(&self) -> Feedback {
        Feedback::Calibrate(self.calibration.clone())
    }
}

struct CaptureState {
    kind: CalibrationKind,
    samples: u32,
    stick_sum: [[i64; 2]; 2],
    stick_min: [[i16; 2]; 2],
    stick_max: [[i16; 2]; 2],
    trigger_min: [u16; 2],
    trigger_max: [u16; 2],
    gyro_sum: [f64; 3],
}

impl CaptureState {
    fn new(kind: CalibrationKind) -> Self {
        Self {
            kind,
            samples: 0,
            stick_sum: [[0; 2]; 2],
            stick_min: [[i16::MAX; 2]; 2],
            stick_max: [[i16::MIN; 2]; 2],
            trigger_min: [u16::MAX; 2],
            trigger_max: [u16::MIN; 2],
            gyro_sum: [0.0; 3],
        }
    }

    fn accept(&mut self, state: &DeviceState) {
        self.samples = self.samples.saturating_add(1);
        let sticks = [state.gamepad.left_stick, state.gamepad.right_stick];
        for (stick, values) in sticks.into_iter().enumerate() {
            for (axis, value) in values.into_iter().enumerate() {
                self.stick_sum[stick][axis] += i64::from(value);
                self.stick_min[stick][axis] = self.stick_min[stick][axis].min(value);
                self.stick_max[stick][axis] = self.stick_max[stick][axis].max(value);
            }
        }
        for (trigger, value) in [state.gamepad.left_trigger, state.gamepad.right_trigger]
            .into_iter()
            .enumerate()
        {
            self.trigger_min[trigger] = self.trigger_min[trigger].min(value);
            self.trigger_max[trigger] = self.trigger_max[trigger].max(value);
        }
        for (sum, value) in self.gyro_sum.iter_mut().zip(state.sensor.gyroscope) {
            *sum += f64::from(value);
        }
    }

    fn progress(&self) -> f32 {
        (self.samples as f32 / 120.0).clamp(0.0, 1.0)
    }

    fn finish(&self, baseline: &Calibration) -> Result<Calibration, CalibrationError> {
        if self.samples == 0 {
            return Err(CalibrationError::InsufficientSamples);
        }
        let mut calibration = baseline.clone();
        match self.kind {
            CalibrationKind::StickCenter => self.finish_stick_center(&mut calibration),
            CalibrationKind::StickRange => self.finish_stick_range(&mut calibration),
            CalibrationKind::TriggerRange => self.finish_trigger_range(&mut calibration),
            CalibrationKind::MotionBias => self.finish_motion_bias(&mut calibration),
        }
        Ok(calibration)
    }

    fn finish_stick_center(&self, calibration: &mut Calibration) {
        for (stick, current) in calibration.sticks.iter_mut().enumerate() {
            let current = current.get_or_insert_default();
            for (axis, center) in current.center.iter_mut().enumerate() {
                let observed = self.stick_sum[stick][axis] / i64::from(self.samples);
                let previous_center = i64::from(*center);
                let range = if observed < 0 {
                    previous_center - i64::from(current.min[axis])
                } else {
                    i64::from(current.max[axis]) - previous_center
                }
                .max(1);
                let correction = observed * range / i64::from(i16::MAX);
                *center = (previous_center + correction)
                    .clamp(i64::from(i16::MIN), i64::from(i16::MAX))
                    as i16;
            }
        }
    }

    fn finish_stick_range(&self, calibration: &mut Calibration) {
        for (stick, current) in calibration.sticks.iter_mut().enumerate() {
            let current = current.get_or_insert_default();
            for axis in 0..2 {
                let center = i64::from(current.center[axis]);
                let negative_range = (center - i64::from(current.min[axis])).max(1);
                let positive_range = (i64::from(current.max[axis]) - center).max(1);
                current.min[axis] = (center
                    + i64::from(self.stick_min[stick][axis]) * negative_range / i64::from(i16::MAX))
                .clamp(i64::from(i16::MIN), i64::from(i16::MAX))
                    as i16;
                current.max[axis] = (center
                    + i64::from(self.stick_max[stick][axis]) * positive_range / i64::from(i16::MAX))
                .clamp(i64::from(i16::MIN), i64::from(i16::MAX))
                    as i16;
            }
        }
    }

    fn finish_trigger_range(&self, calibration: &mut Calibration) {
        for (trigger, current) in calibration.triggers.iter_mut().enumerate() {
            let current = current.get_or_insert(TriggerCalibration::default());
            let min = u64::from(current.min);
            let range = u64::from(current.max.saturating_sub(current.min).max(1));
            current.min = (min + u64::from(self.trigger_min[trigger]) * range / u64::from(u16::MAX))
                .min(u64::from(u16::MAX)) as u16;
            current.max = (min + u64::from(self.trigger_max[trigger]) * range / u64::from(u16::MAX))
                .min(u64::from(u16::MAX)) as u16;
        }
    }

    fn finish_motion_bias(&self, calibration: &mut Calibration) {
        let current = calibration.motion.get_or_insert_default();
        for (axis, bias) in current.gyroscope_bias.iter_mut().enumerate() {
            let observed = self.gyro_sum[axis] / f64::from(self.samples);
            let scale = f64::from(current.gyroscope_scale[axis]);
            if scale.abs() > f64::EPSILON {
                *bias += (observed / scale) as f32;
            }
        }
    }
}

pub struct CalibrationCapture {
    hub: InputHub,
    device: crate::DeviceId,
    active: AtomicBool,
}

impl CalibrationCapture {
    pub(crate) fn new(hub: InputHub, device: crate::DeviceId) -> Self {
        Self {
            hub,
            device,
            active: AtomicBool::new(true),
        }
    }

    pub fn progress(&self) -> f32 {
        self.hub
            .inner
            .calibration_sessions
            .get(&self.device)
            .map_or(0.0, |session| session.progress())
    }

    pub fn finish(self) -> Result<(), CalibrationError> {
        self.active.store(false, Ordering::Release);
        let mut session = self
            .hub
            .inner
            .calibration_sessions
            .get_mut(&self.device)
            .ok_or(CalibrationError::Cancelled)?;
        let feedback = match session.finish() {
            Ok(feedback) => feedback,
            Err(error) => {
                let feedback = session.feedback();
                drop(session);
                let _ = self.apply(&feedback);
                return Err(error);
            }
        };
        drop(session);
        self.apply(&feedback)
            .map_err(|_| CalibrationError::DeviceUnavailable)
    }

    pub fn cancel(self) {
        self.active.store(false, Ordering::Release);
        if let Some(mut session) = self.hub.inner.calibration_sessions.get_mut(&self.device)
            && let Some(feedback) = session.cancel()
        {
            drop(session);
            let _ = self.apply(&feedback);
        }
    }

    fn apply(&self, feedback: &Feedback) -> Result<(), HubError> {
        self.hub.inner.apply_feedback(&self.device, feedback)
    }
}

impl Drop for CalibrationCapture {
    fn drop(&mut self) {
        if self.active.swap(false, Ordering::AcqRel)
            && let Some(mut session) = self.hub.inner.calibration_sessions.get_mut(&self.device)
            && let Some(feedback) = session.cancel()
        {
            drop(session);
            let _ = self.apply(&feedback);
        }
    }
}
