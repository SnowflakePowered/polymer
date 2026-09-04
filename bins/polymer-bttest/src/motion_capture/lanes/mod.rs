use std::io;

use super::{GAMECUBE_INPUT_UUID, MOTION_DATA_OFFSET, MOTION_LENGTH_OFFSET, MotionCapture};

pub mod ui;

const CLASS_C_MOTION_LENGTH: usize = 30;
const CLASS_C_TIMING: u8 = 0x0c;
const FOLLOW_UP_PACKETS: u8 = 3;

const ACTIONS: &[LaneAction] = &[
    LaneAction::new(
        "stationary",
        5,
        "Lay the controller face-up in the reference position and do not touch it.",
        "Leave the controller completely untouched for several seconds.",
    ),
    LaneAction::new(
        "pitch_positive_90",
        5,
        "Start face-up with the shoulder/cable edge pointing away from you.",
        "Rotate +90° pitch, shoulder edge first, and stop with that edge pointing down.",
    ),
    LaneAction::new(
        "pitch_negative_90",
        5,
        "Start face-up with the shoulder/cable edge pointing away from you.",
        "Rotate -90° pitch, grip edge first, and stop with the grip tips pointing down.",
    ),
    LaneAction::new(
        "roll_positive_90",
        5,
        "Start face-up with the shoulder/cable edge pointing away from you.",
        "Rotate +90° roll and stop with the controller's right edge pointing down.",
    ),
    LaneAction::new(
        "roll_negative_90",
        5,
        "Start face-up with the shoulder/cable edge pointing away from you.",
        "Rotate -90° roll and stop with the controller's left edge pointing down.",
    ),
    LaneAction::new(
        "yaw_clockwise_90",
        5,
        "Start face-up with the shoulder/cable edge pointing away from you.",
        "Keep it face-up and rotate 90° clockwise as viewed from above.",
    ),
    LaneAction::new(
        "yaw_counterclockwise_90",
        5,
        "Start face-up with the shoulder/cable edge pointing away from you.",
        "Keep it face-up and rotate 90° counter-clockwise as viewed from above.",
    ),
    LaneAction::new(
        "pitch_closed_loop_360",
        5,
        "Start face-up in the reference position.",
        "Make one complete positive-pitch rotation and return exactly to the reference pose.",
    ),
    LaneAction::new(
        "roll_closed_loop_360",
        5,
        "Start face-up in the reference position.",
        "Make one complete positive-roll rotation and return exactly to the reference pose.",
    ),
    LaneAction::new(
        "yaw_closed_loop_360",
        5,
        "Start face-up in the reference position.",
        "Keep it face-up, make one complete clockwise yaw rotation, and return exactly to the reference pose.",
    ),
    LaneAction::new(
        "translate_x_without_rotation",
        5,
        "Hold the controller face-up and keep its orientation fixed.",
        "Move it briskly left and right several times without rotating it, then stop at the starting pose.",
    ),
    LaneAction::new(
        "translate_y_without_rotation",
        5,
        "Hold the controller face-up and keep its orientation fixed.",
        "Move it briskly toward its shoulders and grips several times without rotating it, then stop at the starting pose.",
    ),
    LaneAction::new(
        "translate_z_without_rotation",
        5,
        "Hold the controller face-up and keep its orientation fixed.",
        "Move it briskly up and down several times without rotating it, then stop at the starting pose.",
    ),
    LaneAction::sleep(
        "sleep_bonded_wake_same_pose",
        "Lay the controller face-up in the reference position and leave it there.",
        "Send the sleep command below. After its LEDs turn off, wake it with HOME without changing its pose, and wait for native reports to resume.",
    ),
];

#[derive(Clone, Copy)]
pub struct LaneAction {
    pub label: &'static str,
    pub repetitions: usize,
    pub positioning_instruction: &'static str,
    pub action_instruction: &'static str,
    pub requests_sleep: bool,
}

impl LaneAction {
    const fn new(
        label: &'static str,
        repetitions: usize,
        positioning_instruction: &'static str,
        action_instruction: &'static str,
    ) -> Self {
        Self {
            label,
            repetitions,
            positioning_instruction,
            action_instruction,
            requests_sleep: false,
        }
    }

    const fn sleep(
        label: &'static str,
        positioning_instruction: &'static str,
        action_instruction: &'static str,
    ) -> Self {
        Self {
            label,
            repetitions: 1,
            positioning_instruction,
            action_instruction,
            requests_sleep: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaneStage {
    Idle,
    Positioning,
    AwaitingStartBoundary,
    Moving,
    AwaitingEndBoundary,
    Holding,
    Complete,
}

#[derive(Clone, Copy, Debug, Default)]
enum State {
    #[default]
    Idle,
    Positioning {
        action: usize,
        repetition: usize,
    },
    AwaitingStartBoundary {
        action: usize,
        repetition: usize,
    },
    Moving {
        action: usize,
        repetition: usize,
    },
    AwaitingEndBoundary {
        action: usize,
        repetition: usize,
    },
    Holding {
        action: usize,
        repetition: usize,
        remaining: u8,
    },
    Complete,
}

#[derive(Clone, Copy)]
pub struct ClassCPacket {
    pub sensor_timestamp: u16,
    pub elapsed_ticks: u16,
    pub lane_control: [u8; 3],
    pub leading_lanes: [u8; 12],
    pub accelerometer_q16_16: [i32; 3],
    pub temperature_raw: i16,
}

pub struct LaneExperimentMarker<'a> {
    pub event: &'a str,
    pub action: &'static str,
    pub action_number: usize,
    pub action_count: usize,
    pub repetition: usize,
    pub repetition_count: usize,
    pub boundary: Option<&'a ClassCPacket>,
}

impl ClassCPacket {
    pub fn parse(characteristic: &str, bytes: &[u8]) -> Option<Self> {
        if characteristic != GAMECUBE_INPUT_UUID
            || usize::from(*bytes.get(MOTION_LENGTH_OFFSET)?) != CLASS_C_MOTION_LENGTH
        {
            return None;
        }
        let motion = bytes.get(MOTION_DATA_OFFSET..MOTION_DATA_OFFSET + CLASS_C_MOTION_LENGTH)?;
        let header = u32::from_le_bytes(motion.get(..4)?.try_into().ok()?);
        if ((header >> 24) & 0x0f) as u8 != CLASS_C_TIMING {
            return None;
        }
        let body = motion.get(4..)?;
        let leading_lanes = body.get(..12)?.try_into().ok()?;
        let accelerometer_q16_16 = [12, 16, 20].map(|offset| {
            i32::from_le_bytes(
                body[offset..offset + 4]
                    .try_into()
                    .expect("validated class-C body"),
            )
        });
        Some(Self {
            sensor_timestamp: (header & 0x0fff) as u16,
            elapsed_ticks: ((header >> 12) & 0x0fff) as u16,
            lane_control: [body[0] & 0b11, body[4] & 0b11, body[8] & 0b11],
            leading_lanes,
            accelerometer_q16_16,
            temperature_raw: i16::from_le_bytes(body[24..26].try_into().ok()?),
        })
    }
}

#[derive(Default)]
pub struct LaneExperiment {
    state: State,
    boundaries_observed: u64,
    first_action: usize,
}

impl LaneExperiment {
    pub fn start(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        self.start_from(capture, 0)
    }

    pub fn start_sleep(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        self.start_from(capture, ACTIONS.len() - 1)
    }

    pub fn restart(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        self.start_from(capture, self.first_action)
    }

    fn start_from(&mut self, capture: &mut MotionCapture, action: usize) -> io::Result<()> {
        self.boundaries_observed = 0;
        self.first_action = action;
        self.state = State::Positioning {
            action,
            repetition: 0,
        };
        self.set_trial_label(capture, "positioning")?;
        self.mark(capture, "lane_experiment_started", None)
    }

    pub fn arm_start(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        let State::Positioning { action, repetition } = self.state else {
            return Ok(());
        };
        self.state = State::AwaitingStartBoundary { action, repetition };
        self.set_trial_label(capture, "awaiting_start_boundary")?;
        self.mark(capture, "lane_start_boundary_armed", None)
    }

    pub fn arm_end(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        let State::Moving { action, repetition } = self.state else {
            return Ok(());
        };
        self.state = State::AwaitingEndBoundary { action, repetition };
        self.mark(capture, "lane_end_boundary_armed", None)
    }

    pub fn observe_class_c(
        &mut self,
        capture: &mut MotionCapture,
        packet: &ClassCPacket,
    ) -> io::Result<()> {
        self.boundaries_observed += 1;
        match self.state {
            State::AwaitingStartBoundary { action, repetition } => {
                self.mark(capture, "lane_movement_started", Some(packet))?;
                self.state = State::Moving { action, repetition };
                self.set_trial_label(capture, "movement")
            }
            State::AwaitingEndBoundary { action, repetition } => {
                self.mark(capture, "lane_movement_finished", Some(packet))?;
                self.state = State::Holding {
                    action,
                    repetition,
                    remaining: FOLLOW_UP_PACKETS,
                };
                self.set_trial_label(capture, "hold")
            }
            State::Holding {
                action,
                repetition,
                remaining,
            } => {
                self.mark(capture, "lane_hold_boundary", Some(packet))?;
                if remaining == 1 {
                    self.mark(capture, "lane_trial_finished", Some(packet))?;
                    self.advance(capture, action, repetition)
                } else {
                    self.state = State::Holding {
                        action,
                        repetition,
                        remaining: remaining - 1,
                    };
                    Ok(())
                }
            }
            State::Idle | State::Positioning { .. } | State::Moving { .. } | State::Complete => {
                Ok(())
            }
        }
    }

    pub fn abort(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        if self.is_running() {
            self.mark(capture, "lane_experiment_aborted", None)?;
        }
        self.state = State::Idle;
        Ok(())
    }

    pub fn reset(&mut self) {
        self.state = State::Idle;
        self.boundaries_observed = 0;
    }

    pub const fn stage(&self) -> LaneStage {
        match self.state {
            State::Idle => LaneStage::Idle,
            State::Positioning { .. } => LaneStage::Positioning,
            State::AwaitingStartBoundary { .. } => LaneStage::AwaitingStartBoundary,
            State::Moving { .. } => LaneStage::Moving,
            State::AwaitingEndBoundary { .. } => LaneStage::AwaitingEndBoundary,
            State::Holding { .. } => LaneStage::Holding,
            State::Complete => LaneStage::Complete,
        }
    }

    pub const fn is_running(&self) -> bool {
        !matches!(self.state, State::Idle | State::Complete)
    }

    pub const fn is_engaged(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    pub fn is_sleep_only(&self) -> bool {
        self.first_action == ACTIONS.len() - 1
    }

    pub fn awaits_sleep_reconnect(&self) -> bool {
        self.current_action()
            .is_some_and(|action| action.requests_sleep)
            && matches!(
                self.state,
                State::Moving { .. } | State::AwaitingEndBoundary { .. }
            )
    }

    pub const fn boundaries_observed(&self) -> u64 {
        self.boundaries_observed
    }

    pub fn current_action(&self) -> Option<&'static LaneAction> {
        self.position().map(|(action, _)| &ACTIONS[action])
    }

    pub fn action_number(&self) -> Option<usize> {
        self.position().map(|(action, _)| action + 1)
    }

    pub const fn action_count(&self) -> usize {
        ACTIONS.len()
    }

    pub fn repetition(&self) -> Option<(usize, usize)> {
        self.position()
            .map(|(action, repetition)| (repetition + 1, ACTIONS[action].repetitions))
    }

    pub fn hold_progress(&self) -> Option<(u8, u8)> {
        let State::Holding { remaining, .. } = self.state else {
            return None;
        };
        Some((FOLLOW_UP_PACKETS - remaining, FOLLOW_UP_PACKETS))
    }

    fn advance(
        &mut self,
        capture: &mut MotionCapture,
        action: usize,
        repetition: usize,
    ) -> io::Result<()> {
        let next_repetition = repetition + 1;
        if next_repetition < ACTIONS[action].repetitions {
            self.state = State::Positioning {
                action,
                repetition: next_repetition,
            };
            return self.set_trial_label(capture, "positioning");
        }
        let next_action = action + 1;
        if next_action < ACTIONS.len() {
            self.state = State::Positioning {
                action: next_action,
                repetition: 0,
            };
            return self.set_trial_label(capture, "positioning");
        }
        capture.set_label("lane_experiment_complete")?;
        self.mark(capture, "lane_experiment_finished", None)?;
        self.state = State::Complete;
        Ok(())
    }

    fn set_trial_label(&self, capture: &mut MotionCapture, stage: &str) -> io::Result<()> {
        let Some((action, repetition)) = self.position() else {
            return Ok(());
        };
        capture.set_label(&format!(
            "lane_{stage}_{}_r{:02}",
            ACTIONS[action].label,
            repetition + 1
        ))
    }

    fn mark(
        &self,
        capture: &mut MotionCapture,
        event: &str,
        boundary: Option<&ClassCPacket>,
    ) -> io::Result<()> {
        let (action, repetition) = self.position().unwrap_or((ACTIONS.len() - 1, 0));
        let definition = &ACTIONS[action];
        capture.mark_lane_experiment(LaneExperimentMarker {
            event,
            action: definition.label,
            action_number: action + 1,
            action_count: ACTIONS.len(),
            repetition: repetition + 1,
            repetition_count: definition.repetitions,
            boundary,
        })
    }

    const fn position(&self) -> Option<(usize, usize)> {
        match self.state {
            State::Positioning { action, repetition }
            | State::AwaitingStartBoundary { action, repetition }
            | State::Moving { action, repetition }
            | State::AwaitingEndBoundary { action, repetition }
            | State::Holding {
                action, repetition, ..
            } => Some((action, repetition)),
            State::Idle | State::Complete => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ClassCPacket, GAMECUBE_INPUT_UUID};

    #[test]
    fn captured_class_c_packet_decodes_boundary_state() {
        let report = [
            0x5d, 0x1c, 0x00, 0x00, 0x00, 0x5e, 0x48, 0x7f, 0xf9, 0x57, 0x81, 0x38, 0x1f, 0x20,
            0x1e, 0x9a, 0x1a, 0x00, 0x0c, 0x01, 0xb0, 0x49, 0x26, 0x02, 0xf2, 0x79, 0x24, 0x01,
            0xaa, 0xa5, 0x0a, 0xb4, 0x45, 0x1f, 0x02, 0x50, 0xd0, 0xdc, 0xfc, 0x40, 0xac, 0x4d,
            0x11, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        let packet = ClassCPacket::parse(GAMECUBE_INPUT_UUID, &report).unwrap();

        assert_eq!(packet.sensor_timestamp, 2714);
        assert_eq!(packet.elapsed_ticks, 1);
        assert_eq!(packet.lane_control, [1, 2, 1]);
        assert_eq!(
            packet.accelerometer_q16_16,
            [35_603_892, -52_637_616, 290_303_040]
        );
        assert_eq!(packet.temperature_raw, 192);
    }
}
