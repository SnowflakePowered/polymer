use std::io;

use super::{GAMECUBE_INPUT_UUID, MOTION_DATA_OFFSET, MOTION_LENGTH_OFFSET, MotionCapture};

pub mod ui;

const MOTION_LENGTH: usize = 40;
const TIMING_CLASS: u8 = 0x0d;
const PACKETS_PER_ACTION: u16 = 8;

const ACTIONS: &[ClassDAction] = &[
    ClassDAction::new(
        "stationary_face_up",
        "Lay the controller face-up with its shoulder edge pointing away from you.",
        "Keep it completely still until all class-D packets are collected.",
    ),
    ClassDAction::new(
        "stationary_face_down",
        "Lay the controller face-down with its shoulder edge pointing away from you.",
        "Keep it completely still until all class-D packets are collected.",
    ),
    ClassDAction::new(
        "stationary_left_edge_down",
        "Rest the controller on its left edge.",
        "Keep it completely still until all class-D packets are collected.",
    ),
    ClassDAction::new(
        "stationary_right_edge_down",
        "Rest the controller on its right edge.",
        "Keep it completely still until all class-D packets are collected.",
    ),
    ClassDAction::new(
        "stationary_grip_edge_down",
        "Hold it vertically with the grip tips pointing down.",
        "Keep it completely still until all class-D packets are collected.",
    ),
    ClassDAction::new(
        "stationary_shoulder_edge_down",
        "Hold it vertically with the shoulder/cable edge pointing down.",
        "Keep it completely still until all class-D packets are collected.",
    ),
    ClassDAction::new(
        "pitch_positive_slow",
        "Hold the controller clear of the desk.",
        "Repeatedly make smooth positive-pitch rotations, shoulder edge first, then return slowly.",
    ),
    ClassDAction::new(
        "pitch_negative_slow",
        "Hold the controller clear of the desk.",
        "Repeatedly make smooth negative-pitch rotations, grip edge first, then return slowly.",
    ),
    ClassDAction::new(
        "roll_positive_slow",
        "Hold the controller clear of the desk.",
        "Repeatedly roll it right-edge first at a steady speed, then return slowly.",
    ),
    ClassDAction::new(
        "roll_negative_slow",
        "Hold the controller clear of the desk.",
        "Repeatedly roll it left-edge first at a steady speed, then return slowly.",
    ),
    ClassDAction::new(
        "yaw_clockwise_slow",
        "Hold it approximately face-up.",
        "Repeatedly rotate it clockwise at a steady speed as viewed from above, then return slowly.",
    ),
    ClassDAction::new(
        "yaw_counterclockwise_slow",
        "Hold it approximately face-up.",
        "Repeatedly rotate it counter-clockwise at a steady speed as viewed from above, then return slowly.",
    ),
    ClassDAction::new(
        "pitch_flicks",
        "Hold the controller clear of the desk.",
        "Make isolated pitch flicks in both directions, pausing briefly between flicks.",
    ),
    ClassDAction::new(
        "roll_flicks",
        "Hold the controller clear of the desk.",
        "Make isolated roll flicks in both directions, pausing briefly between flicks.",
    ),
    ClassDAction::new(
        "yaw_flicks",
        "Hold the controller clear of the desk.",
        "Make isolated yaw flicks in both directions, pausing briefly between flicks.",
    ),
    ClassDAction::new(
        "translate_x_without_rotation",
        "Hold it face-up and keep its orientation fixed.",
        "Move it briskly left and right without intentionally rotating it.",
    ),
    ClassDAction::new(
        "translate_y_without_rotation",
        "Hold it face-up and keep its orientation fixed.",
        "Move it briskly between its shoulder and grip directions without intentionally rotating it.",
    ),
    ClassDAction::new(
        "translate_z_without_rotation",
        "Hold it face-up and keep its orientation fixed.",
        "Move it briskly up and down without intentionally rotating it.",
    ),
];

#[derive(Clone, Copy)]
pub struct ClassDAction {
    pub label: &'static str,
    pub positioning_instruction: &'static str,
    pub capture_instruction: &'static str,
}

impl ClassDAction {
    const fn new(
        label: &'static str,
        positioning_instruction: &'static str,
        capture_instruction: &'static str,
    ) -> Self {
        Self {
            label,
            positioning_instruction,
            capture_instruction,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClassDStage {
    Idle,
    Positioning,
    Capturing,
    Complete,
}

#[derive(Clone, Copy, Debug, Default)]
enum State {
    #[default]
    Idle,
    Positioning {
        action: usize,
    },
    Capturing {
        action: usize,
        observed: u16,
    },
    Complete,
}

#[derive(Clone, Copy)]
pub struct ClassDPacket {
    pub sensor_timestamp: u16,
    pub elapsed_ticks: u16,
    pub range_flags: u8,
    pub omitted_component: u8,
    pub reserved_bit_4: u8,
    pub accelerometer_first: [i16; 3],
    pub gyroscope_terminal: [i16; 3],
    pub accelerometer_terminal: [i16; 3],
    pub temperature_raw: i16,
    pub unresolved_prefix: [u8; 11],
    pub unresolved_interleaved: [u8; 8],
    pub body: [u8; 36],
}

impl ClassDPacket {
    pub fn parse(characteristic: &str, bytes: &[u8]) -> Option<Self> {
        if characteristic != GAMECUBE_INPUT_UUID
            || usize::from(*bytes.get(MOTION_LENGTH_OFFSET)?) != MOTION_LENGTH
        {
            return None;
        }
        let motion = bytes.get(MOTION_DATA_OFFSET..MOTION_DATA_OFFSET + MOTION_LENGTH)?;
        let header = u32::from_le_bytes(motion.get(..4)?.try_into().ok()?);
        if ((header >> 24) & 0x0f) as u8 != TIMING_CLASS {
            return None;
        }
        let body: [u8; 36] = motion.get(4..)?.try_into().ok()?;
        let mut unresolved_prefix = [0_u8; 11];
        unresolved_prefix.copy_from_slice(&body[..11]);
        unresolved_prefix[10] &= 0b0000_0011;
        Some(Self {
            sensor_timestamp: (header & 0x0fff) as u16,
            elapsed_ticks: ((header >> 12) & 0x0fff) as u16,
            range_flags: body[0] & 0b11,
            omitted_component: (body[0] >> 2) & 0b11,
            reserved_bit_4: (body[0] >> 4) & 1,
            accelerometer_first: Self::vector(&body, 82),
            gyroscope_terminal: Self::vector(&body, 148),
            accelerometer_terminal: Self::vector(&body, 214),
            temperature_raw: Self::read_signed(&body, 272, 16) as i16,
            unresolved_prefix,
            unresolved_interleaved: [96, 118, 140, 162, 184, 206, 228, 250]
                .map(|offset| Self::read_unsigned(&body, offset, 8) as u8),
            body,
        })
    }

    fn vector(body: &[u8], offset: usize) -> [i16; 3] {
        std::array::from_fn(|axis| Self::read_signed(body, offset + axis * 22, 14) as i16)
    }

    fn read_signed(body: &[u8], offset: usize, width: usize) -> i32 {
        let shift = u32::BITS as usize - width;
        ((Self::read_unsigned(body, offset, width) << shift) as i32) >> shift
    }

    fn read_unsigned(body: &[u8], offset: usize, width: usize) -> u32 {
        let mut value = 0_u32;
        for bit in 0..width {
            let source = offset + bit;
            value |= u32::from((body[source / 8] >> (source & 7)) & 1) << bit;
        }
        value
    }
}

pub struct ClassDExperimentMarker<'a> {
    pub event: &'a str,
    pub action: &'static str,
    pub action_number: usize,
    pub action_count: usize,
    pub observed: u16,
    pub target: u16,
    pub packet: Option<&'a ClassDPacket>,
}

#[derive(Default)]
pub struct ClassDExperiment {
    state: State,
    total_observed: u64,
}

impl ClassDExperiment {
    pub fn start(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        self.total_observed = 0;
        self.state = State::Positioning { action: 0 };
        self.set_label(capture, "positioning")?;
        self.mark(capture, "class_d_experiment_started", 0, None)
    }

    pub fn begin_action(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        let State::Positioning { action } = self.state else {
            return Ok(());
        };
        self.state = State::Capturing {
            action,
            observed: 0,
        };
        self.set_label(capture, "capturing")?;
        self.mark(capture, "class_d_action_started", 0, None)
    }

    pub fn observe(
        &mut self,
        capture: &mut MotionCapture,
        packet: &ClassDPacket,
    ) -> io::Result<()> {
        let State::Capturing { action, observed } = self.state else {
            return Ok(());
        };
        let observed = observed + 1;
        self.total_observed += 1;
        self.state = State::Capturing { action, observed };
        self.mark(capture, "class_d_packet", observed, Some(packet))?;
        if observed < PACKETS_PER_ACTION {
            return Ok(());
        }
        self.mark(capture, "class_d_action_finished", observed, None)?;
        let next_action = action + 1;
        if next_action == ACTIONS.len() {
            capture.set_label("class_d_experiment_complete")?;
            self.mark(capture, "class_d_experiment_finished", observed, None)?;
            self.state = State::Complete;
        } else {
            self.state = State::Positioning {
                action: next_action,
            };
            self.set_label(capture, "positioning")?;
        }
        Ok(())
    }

    pub fn restart(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        self.start(capture)
    }

    pub fn abort(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        if self.is_running() {
            let observed = self.observed_in_action();
            self.mark(capture, "class_d_experiment_aborted", observed, None)?;
        }
        self.state = State::Idle;
        Ok(())
    }

    pub fn reset(&mut self) {
        self.state = State::Idle;
        self.total_observed = 0;
    }

    pub const fn stage(&self) -> ClassDStage {
        match self.state {
            State::Idle => ClassDStage::Idle,
            State::Positioning { .. } => ClassDStage::Positioning,
            State::Capturing { .. } => ClassDStage::Capturing,
            State::Complete => ClassDStage::Complete,
        }
    }

    pub const fn is_running(&self) -> bool {
        matches!(
            self.state,
            State::Positioning { .. } | State::Capturing { .. }
        )
    }

    pub const fn is_engaged(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    pub fn current_action(&self) -> Option<&'static ClassDAction> {
        self.action_index().map(|action| &ACTIONS[action])
    }

    pub fn action_number(&self) -> Option<usize> {
        self.action_index().map(|action| action + 1)
    }

    pub const fn action_count(&self) -> usize {
        ACTIONS.len()
    }

    pub const fn target_per_action(&self) -> u16 {
        PACKETS_PER_ACTION
    }

    pub const fn total_observed(&self) -> u64 {
        self.total_observed
    }

    pub const fn observed_in_action(&self) -> u16 {
        match self.state {
            State::Capturing { observed, .. } => observed,
            State::Idle | State::Positioning { .. } | State::Complete => 0,
        }
    }

    fn set_label(&self, capture: &mut MotionCapture, stage: &str) -> io::Result<()> {
        let Some(action) = self.action_index() else {
            return Ok(());
        };
        capture.set_label(&format!("class_d_{stage}_{}", ACTIONS[action].label))
    }

    fn mark(
        &self,
        capture: &mut MotionCapture,
        event: &str,
        observed: u16,
        packet: Option<&ClassDPacket>,
    ) -> io::Result<()> {
        let action = self.action_index().unwrap_or(ACTIONS.len() - 1);
        capture.mark_class_d_experiment(ClassDExperimentMarker {
            event,
            action: ACTIONS[action].label,
            action_number: action + 1,
            action_count: ACTIONS.len(),
            observed,
            target: PACKETS_PER_ACTION,
            packet,
        })
    }

    const fn action_index(&self) -> Option<usize> {
        match self.state {
            State::Positioning { action } | State::Capturing { action, .. } => Some(action),
            State::Idle | State::Complete => None,
        }
    }
}
