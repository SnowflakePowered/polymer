use std::{
    io,
    time::{Duration, Instant},
};

use super::MotionCapture;

pub mod ui;

pub const HOLD_DURATION: Duration = Duration::from_secs(5);

const STEPS: &[QuaternionStep] = &[
    QuaternionStep::new(
        "quaternion_position_reference_face_up",
        "quaternion_hold_reference_face_up",
        "Lay the controller face-up. Point its shoulder/cable edge toward the top of your paper and its grips toward you.",
    ),
    QuaternionStep::new(
        "quaternion_transition_yaw_clockwise_000_to_090",
        "quaternion_hold_yaw_clockwise_090",
        "Keep it face-up and rotate it 90° clockwise as viewed from directly above.",
    ),
    QuaternionStep::new(
        "quaternion_transition_yaw_clockwise_090_to_180",
        "quaternion_hold_yaw_clockwise_180",
        "Keep it face-up and rotate another 90° clockwise, for 180° total.",
    ),
    QuaternionStep::new(
        "quaternion_transition_yaw_clockwise_180_to_270",
        "quaternion_hold_yaw_clockwise_270",
        "Keep it face-up and rotate another 90° clockwise, for 270° total.",
    ),
    QuaternionStep::new(
        "quaternion_transition_yaw_clockwise_270_to_360",
        "quaternion_hold_yaw_clockwise_360",
        "Keep it face-up and rotate another 90° clockwise. It should now match the reference outline.",
    ),
    QuaternionStep::new(
        "quaternion_transition_yaw_counterclockwise_000_to_090",
        "quaternion_hold_yaw_counterclockwise_090",
        "From the face-up reference, rotate 90° counter-clockwise as viewed from directly above.",
    ),
    QuaternionStep::new(
        "quaternion_transition_yaw_counterclockwise_090_to_180",
        "quaternion_hold_yaw_counterclockwise_180",
        "Keep it face-up and rotate another 90° counter-clockwise, for 180° total.",
    ),
    QuaternionStep::new(
        "quaternion_transition_yaw_counterclockwise_180_to_270",
        "quaternion_hold_yaw_counterclockwise_270",
        "Keep it face-up and rotate another 90° counter-clockwise, for 270° total.",
    ),
    QuaternionStep::new(
        "quaternion_transition_yaw_counterclockwise_270_to_360",
        "quaternion_hold_yaw_counterclockwise_360",
        "Keep it face-up and rotate another 90° counter-clockwise. It should now match the reference outline.",
    ),
    QuaternionStep::new(
        "quaternion_transition_pitch_shoulder_first_000_to_090",
        "quaternion_hold_pitch_shoulder_first_090",
        "From face-up, rotate about the controller's left-to-right axis until the shoulder/cable edge points down.",
    ),
    QuaternionStep::new(
        "quaternion_transition_pitch_shoulder_first_090_to_180",
        "quaternion_hold_pitch_shoulder_first_180",
        "Continue in the same direction by 90°. The controller should now be face-down.",
    ),
    QuaternionStep::new(
        "quaternion_transition_pitch_shoulder_first_180_to_270",
        "quaternion_hold_pitch_shoulder_first_270",
        "Continue in the same direction by 90°. The grip edge should now point down.",
    ),
    QuaternionStep::new(
        "quaternion_transition_pitch_shoulder_first_270_to_360",
        "quaternion_hold_pitch_shoulder_first_360",
        "Continue in the same direction by 90°. Return exactly to the face-up reference.",
    ),
    QuaternionStep::new(
        "quaternion_transition_pitch_grip_first_000_to_090",
        "quaternion_hold_pitch_grip_first_090",
        "From face-up, rotate about the controller's left-to-right axis until the grip edge points down.",
    ),
    QuaternionStep::new(
        "quaternion_transition_pitch_grip_first_090_to_180",
        "quaternion_hold_pitch_grip_first_180",
        "Continue in the same direction by 90°. The controller should now be face-down.",
    ),
    QuaternionStep::new(
        "quaternion_transition_pitch_grip_first_180_to_270",
        "quaternion_hold_pitch_grip_first_270",
        "Continue in the same direction by 90°. The shoulder/cable edge should now point down.",
    ),
    QuaternionStep::new(
        "quaternion_transition_pitch_grip_first_270_to_360",
        "quaternion_hold_pitch_grip_first_360",
        "Continue in the same direction by 90°. Return exactly to the face-up reference.",
    ),
    QuaternionStep::new(
        "quaternion_transition_roll_right_first_000_to_090",
        "quaternion_hold_roll_right_first_090",
        "From face-up, rotate about the shoulder-to-grip axis until the controller's right edge points down.",
    ),
    QuaternionStep::new(
        "quaternion_transition_roll_right_first_090_to_180",
        "quaternion_hold_roll_right_first_180",
        "Continue in the same direction by 90°. The controller should now be face-down.",
    ),
    QuaternionStep::new(
        "quaternion_transition_roll_right_first_180_to_270",
        "quaternion_hold_roll_right_first_270",
        "Continue in the same direction by 90°. The controller's left edge should now point down.",
    ),
    QuaternionStep::new(
        "quaternion_transition_roll_right_first_270_to_360",
        "quaternion_hold_roll_right_first_360",
        "Continue in the same direction by 90°. Return exactly to the face-up reference.",
    ),
    QuaternionStep::new(
        "quaternion_transition_roll_left_first_000_to_090",
        "quaternion_hold_roll_left_first_090",
        "From face-up, rotate about the shoulder-to-grip axis until the controller's left edge points down.",
    ),
    QuaternionStep::new(
        "quaternion_transition_roll_left_first_090_to_180",
        "quaternion_hold_roll_left_first_180",
        "Continue in the same direction by 90°. The controller should now be face-down.",
    ),
    QuaternionStep::new(
        "quaternion_transition_roll_left_first_180_to_270",
        "quaternion_hold_roll_left_first_270",
        "Continue in the same direction by 90°. The controller's right edge should now point down.",
    ),
    QuaternionStep::new(
        "quaternion_transition_roll_left_first_270_to_360",
        "quaternion_hold_roll_left_first_360",
        "Continue in the same direction by 90°. Return exactly to the face-up reference.",
    ),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuaternionStage {
    Idle,
    Positioning,
    Holding,
    Complete,
}

#[derive(Clone, Copy)]
pub struct QuaternionStep {
    pub transition_label: &'static str,
    pub hold_label: &'static str,
    pub instruction: &'static str,
}

impl QuaternionStep {
    const fn new(
        transition_label: &'static str,
        hold_label: &'static str,
        instruction: &'static str,
    ) -> Self {
        Self {
            transition_label,
            hold_label,
            instruction,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
enum State {
    #[default]
    Idle,
    Positioning {
        step: usize,
    },
    Holding {
        step: usize,
        started_at: Instant,
        deadline: Instant,
    },
    Complete,
}

#[derive(Default)]
pub struct QuaternionExperiment {
    state: State,
}

impl QuaternionExperiment {
    pub fn start(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        capture.set_label(STEPS[0].transition_label)?;
        capture.mark("quaternion_experiment_started")?;
        self.state = State::Positioning { step: 0 };
        Ok(())
    }

    pub fn begin_hold(&mut self, capture: &mut MotionCapture, now: Instant) -> io::Result<()> {
        let State::Positioning { step } = self.state else {
            return Ok(());
        };
        capture.set_label(STEPS[step].hold_label)?;
        capture.mark("quaternion_hold_started")?;
        self.state = State::Holding {
            step,
            started_at: now,
            deadline: now + HOLD_DURATION,
        };
        Ok(())
    }

    pub fn tick(&mut self, capture: &mut MotionCapture, now: Instant) -> io::Result<()> {
        let State::Holding { step, deadline, .. } = self.state else {
            return Ok(());
        };
        if now < deadline {
            return Ok(());
        }

        capture.mark("quaternion_hold_finished")?;
        let next_step = step + 1;
        if next_step == STEPS.len() {
            capture.set_label("quaternion_complete")?;
            capture.mark("quaternion_experiment_finished")?;
            self.state = State::Complete;
            return Ok(());
        }

        capture.set_label(STEPS[next_step].transition_label)?;
        capture.mark("quaternion_transition_started")?;
        self.state = State::Positioning { step: next_step };
        Ok(())
    }

    pub fn abort(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        if !self.is_running() {
            return Ok(());
        }
        capture.mark("quaternion_experiment_aborted")?;
        self.state = State::Idle;
        Ok(())
    }

    pub fn reset(&mut self) {
        self.state = State::Idle;
    }

    pub const fn stage(&self) -> QuaternionStage {
        match self.state {
            State::Idle => QuaternionStage::Idle,
            State::Positioning { .. } => QuaternionStage::Positioning,
            State::Holding { .. } => QuaternionStage::Holding,
            State::Complete => QuaternionStage::Complete,
        }
    }

    pub const fn is_running(&self) -> bool {
        matches!(
            self.state,
            State::Positioning { .. } | State::Holding { .. }
        )
    }

    pub const fn is_engaged(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    pub fn current_step(&self) -> Option<&'static QuaternionStep> {
        let step = match self.state {
            State::Positioning { step } | State::Holding { step, .. } => step,
            State::Idle | State::Complete => return None,
        };
        STEPS.get(step)
    }

    pub const fn step_number(&self) -> Option<usize> {
        match self.state {
            State::Positioning { step } | State::Holding { step, .. } => Some(step + 1),
            State::Idle | State::Complete => None,
        }
    }

    pub const fn step_count(&self) -> usize {
        STEPS.len()
    }

    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        let State::Holding { deadline, .. } = self.state else {
            return None;
        };
        Some(deadline.saturating_duration_since(now))
    }

    pub fn hold_progress(&self, now: Instant) -> Option<f32> {
        let State::Holding {
            started_at,
            deadline,
            ..
        } = self.state
        else {
            return None;
        };
        let total = deadline.duration_since(started_at).as_secs_f32();
        let elapsed = now.saturating_duration_since(started_at).as_secs_f32();
        Some((elapsed / total).clamp(0.0, 1.0))
    }
}
