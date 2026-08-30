use std::{io, time::Instant};

use super::{
    GAMECUBE_INPUT_UUID, MOTION_DATA_OFFSET, MOTION_LENGTH_OFFSET, MotionCapture,
    lanes::ClassCPacket,
};

pub mod ui;

const FULL_MOTION_LENGTH: usize = 40;
const NORMAL_TIMING_CLASS: u8 = 0x0e;
const STATIONARY_CONTEXTS: u16 = 12;
const DYNAMIC_CONTEXTS: u16 = 24;

const ACTIONS: &[AccelerationAction] = &[
    AccelerationAction::new(
        "stationary_baseline",
        "Stationary baseline",
        STATIONARY_CONTEXTS,
        "Lay the controller face-up on a firm, level surface.",
        "Do not touch the controller. This records the class-C noise floor.",
    ),
    AccelerationAction::translation(
        "translate_x_slow",
        "X translation · slow",
        "Move it left and right in one broad cycle per second.",
    ),
    AccelerationAction::translation(
        "translate_x_medium",
        "X translation · medium",
        "Move it left and right in about three small cycles per second.",
    ),
    AccelerationAction::translation(
        "translate_x_fast",
        "X translation · fast",
        "Move it left and right in the fastest small, controlled oscillation you can sustain (about 5–7 cycles per second).",
    ),
    AccelerationAction::translation(
        "translate_y_slow",
        "Y translation · slow",
        "Move it between its shoulder and grip directions in one broad cycle per second.",
    ),
    AccelerationAction::translation(
        "translate_y_medium",
        "Y translation · medium",
        "Move it between its shoulder and grip directions in about three small cycles per second.",
    ),
    AccelerationAction::translation(
        "translate_y_fast",
        "Y translation · fast",
        "Move it between its shoulder and grip directions in the fastest small, controlled oscillation you can sustain (about 5–7 cycles per second).",
    ),
    AccelerationAction::translation(
        "translate_z_slow",
        "Z translation · slow",
        "Move it vertically up and down in one broad cycle per second.",
    ),
    AccelerationAction::translation(
        "translate_z_medium",
        "Z translation · medium",
        "Move it vertically up and down in about three small cycles per second.",
    ),
    AccelerationAction::translation(
        "translate_z_fast",
        "Z translation · fast",
        "Move it vertically up and down in the fastest small, controlled oscillation you can sustain (about 5–7 cycles per second).",
    ),
    AccelerationAction::impulses(
        "impulse_x",
        "X isolated impulses",
        "Make a short, sharp left/right slide, stop completely, wait about one second, then repeat in the other direction.",
    ),
    AccelerationAction::impulses(
        "impulse_y",
        "Y isolated impulses",
        "Make a short, sharp shoulder/grip-direction slide, stop completely, wait about one second, then repeat in the other direction.",
    ),
    AccelerationAction::impulses(
        "impulse_z",
        "Z isolated impulses",
        "Make a short, sharp vertical movement, stop completely, wait about one second, then repeat in the other direction.",
    ),
];

#[derive(Clone, Copy)]
pub struct AccelerationAction {
    pub id: &'static str,
    pub title: &'static str,
    pub target: u16,
    pub positioning_instruction: &'static str,
    pub capture_instruction: &'static str,
}

impl AccelerationAction {
    const fn new(
        id: &'static str,
        title: &'static str,
        target: u16,
        positioning_instruction: &'static str,
        capture_instruction: &'static str,
    ) -> Self {
        Self {
            id,
            title,
            target,
            positioning_instruction,
            capture_instruction,
        }
    }

    const fn translation(
        id: &'static str,
        title: &'static str,
        capture_instruction: &'static str,
    ) -> Self {
        Self::new(
            id,
            title,
            DYNAMIC_CONTEXTS,
            "Hold the controller face-up and keep its orientation fixed. Use both hands and a small range of travel if that helps prevent tilting.",
            capture_instruction,
        )
    }

    const fn impulses(
        id: &'static str,
        title: &'static str,
        capture_instruction: &'static str,
    ) -> Self {
        Self::new(
            id,
            title,
            DYNAMIC_CONTEXTS,
            "Hold the controller face-up with its orientation fixed and leave enough room for a short translation followed by a clean stop.",
            capture_instruction,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccelerationStage {
    Idle,
    Positioning,
    Capturing,
    Complete,
}

#[derive(Clone, Copy, Debug, Default)]
enum State {
    #[default]
    Idle,
    Positioning { action: usize, observed: u16 },
    Capturing { action: usize, observed: u16 },
    Complete,
}

#[derive(Clone, Copy)]
pub struct NormalAccelerationPacket {
    pub sensor_timestamp: u16,
    pub elapsed_ticks: u16,
    pub range_flags: u8,
    pub earlier: [i32; 3],
    pub middle: [i32; 3],
    pub terminal: [i32; 3],
}

impl NormalAccelerationPacket {
    fn parse(characteristic: &str, bytes: &[u8]) -> Option<Self> {
        if characteristic != GAMECUBE_INPUT_UUID
            || usize::from(*bytes.get(MOTION_LENGTH_OFFSET)?) != FULL_MOTION_LENGTH
        {
            return None;
        }
        let motion = bytes.get(MOTION_DATA_OFFSET..MOTION_DATA_OFFSET + FULL_MOTION_LENGTH)?;
        let header = u32::from_le_bytes(motion.get(..4)?.try_into().ok()?);
        if ((header >> 24) & 0x0f) as u8 != NORMAL_TIMING_CLASS {
            return None;
        }
        let body = PackedAccelerationBody::new(motion.get(4..)?);
        let range_flags = body.read_unsigned(0, 2) as u8;
        let range_shift = if range_flags & 1 == 0 { 2 } else { 0 };
        Some(Self {
            sensor_timestamp: (header & 0x0fff) as u16,
            elapsed_ticks: ((header >> 12) & 0x0fff) as u16,
            range_flags,
            earlier: body.raw_vector(68, 14, 14, range_shift),
            middle: body.raw_vector(149, 13, 13, 1 + range_shift),
            terminal: body.raw_vector(230, 14, 14, range_shift),
        })
    }
}

#[derive(Clone, Copy)]
pub enum AccelerationPacket {
    Normal(NormalAccelerationPacket),
    Precision(ClassCPacket),
}

impl AccelerationPacket {
    pub fn parse(characteristic: &str, bytes: &[u8]) -> Option<Self> {
        ClassCPacket::parse(characteristic, bytes)
            .map(Self::Precision)
            .or_else(|| NormalAccelerationPacket::parse(characteristic, bytes).map(Self::Normal))
    }
}

#[derive(Clone, Copy)]
struct Observation<T> {
    notification_sequence: u64,
    packet: T,
    received_at: Instant,
}

#[derive(Clone, Copy)]
struct PendingContext {
    previous: Observation<NormalAccelerationPacket>,
    focus: Observation<ClassCPacket>,
}

#[derive(Clone, Copy)]
pub struct AccelerationPoint<T> {
    pub notification_sequence: u64,
    pub packet: T,
}

#[derive(Clone, Copy)]
pub struct ClassCAccelerationContext {
    pub previous: AccelerationPoint<NormalAccelerationPacket>,
    pub focus: AccelerationPoint<ClassCPacket>,
    pub next: AccelerationPoint<NormalAccelerationPacket>,
    pub normal_span_ticks: u16,
    pub first_sample_tick: u16,
    pub second_sample_tick: u16,
    pub previous_to_focus_sensor_ticks: u16,
    pub focus_to_next_sensor_ticks: u16,
    pub previous_to_focus_host_ns: u128,
    pub focus_to_next_host_ns: u128,
}

pub struct AccelerationExperimentMarker {
    pub event: &'static str,
    pub action: &'static AccelerationAction,
    pub observed: u16,
    pub total_observed: u64,
    pub rejected: u64,
    pub context: Option<ClassCAccelerationContext>,
}

#[derive(Default)]
pub struct ClassCAccelerationExperiment {
    state: State,
    previous_normal: Option<Observation<NormalAccelerationPacket>>,
    pending: Vec<PendingContext>,
    total_observed: u64,
    rejected: u64,
}

impl ClassCAccelerationExperiment {
    pub fn start(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        self.reset();
        self.state = State::Positioning {
            action: 0,
            observed: 0,
        };
        self.set_label(capture, "positioning")?;
        self.mark(capture, "class_c_acceleration_experiment_started", None)
    }

    pub fn begin_action(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        let State::Positioning { action, observed } = self.state else {
            return Ok(());
        };
        self.clear_context_window();
        self.state = State::Capturing { action, observed };
        self.set_label(capture, "capturing")?;
        self.mark(capture, "class_c_acceleration_action_started", None)
    }

    pub fn pause_action(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        let State::Capturing { action, observed } = self.state else {
            return Ok(());
        };
        self.clear_context_window();
        self.state = State::Positioning { action, observed };
        self.set_label(capture, "positioning")?;
        self.mark(capture, "class_c_acceleration_action_paused", None)
    }

    pub fn observe(
        &mut self,
        capture: &mut MotionCapture,
        packet: AccelerationPacket,
        received_at: Instant,
    ) -> io::Result<()> {
        if !self.is_engaged() {
            return Ok(());
        }
        match packet {
            AccelerationPacket::Normal(packet) => {
                self.observe_normal(capture, packet, received_at)
            }
            AccelerationPacket::Precision(packet) => {
                self.observe_precision(capture, packet, received_at);
                Ok(())
            }
        }
    }

    pub fn restart(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        self.start(capture)
    }

    pub fn abort(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        if self.is_running() {
            self.mark(capture, "class_c_acceleration_experiment_aborted", None)?;
        }
        self.state = State::Idle;
        self.clear_context_window();
        Ok(())
    }

    pub fn finish(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        if self.is_engaged() {
            self.mark(capture, "class_c_acceleration_experiment_summary", None)?;
        }
        self.state = State::Idle;
        self.clear_context_window();
        Ok(())
    }

    pub fn reset(&mut self) {
        self.state = State::Idle;
        self.clear_context_window();
        self.total_observed = 0;
        self.rejected = 0;
    }

    pub const fn stage(&self) -> AccelerationStage {
        match self.state {
            State::Idle => AccelerationStage::Idle,
            State::Positioning { .. } => AccelerationStage::Positioning,
            State::Capturing { .. } => AccelerationStage::Capturing,
            State::Complete => AccelerationStage::Complete,
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

    pub fn current_action(&self) -> Option<&'static AccelerationAction> {
        self.action_index().map(|action| &ACTIONS[action])
    }

    pub fn action_number(&self) -> Option<usize> {
        self.action_index().map(|action| action + 1)
    }

    pub const fn action_count(&self) -> usize {
        ACTIONS.len()
    }

    pub const fn observed_in_action(&self) -> u16 {
        match self.state {
            State::Positioning { observed, .. } | State::Capturing { observed, .. } => observed,
            State::Idle | State::Complete => 0,
        }
    }

    pub const fn total_observed(&self) -> u64 {
        self.total_observed
    }

    pub const fn rejected(&self) -> u64 {
        self.rejected
    }

    fn observe_normal(
        &mut self,
        capture: &mut MotionCapture,
        packet: NormalAccelerationPacket,
        received_at: Instant,
    ) -> io::Result<()> {
        let current = Observation {
            notification_sequence: capture.samples(),
            packet,
            received_at,
        };
        if matches!(self.state, State::Capturing { .. }) {
            for pending in std::mem::take(&mut self.pending) {
                let Some(context) = pending.complete(current) else {
                    self.rejected += 1;
                    continue;
                };
                self.record_context(capture, context)?;
                if !matches!(self.state, State::Capturing { .. }) {
                    break;
                }
            }
        } else {
            self.pending.clear();
        }
        self.previous_normal = Some(current);
        Ok(())
    }

    fn observe_precision(
        &mut self,
        capture: &MotionCapture,
        packet: ClassCPacket,
        received_at: Instant,
    ) {
        let (State::Capturing { .. }, Some(previous)) = (self.state, self.previous_normal) else {
            return;
        };
        self.pending.push(PendingContext {
            previous,
            focus: Observation {
                notification_sequence: capture.samples(),
                packet,
                received_at,
            },
        });
    }

    fn record_context(
        &mut self,
        capture: &mut MotionCapture,
        context: ClassCAccelerationContext,
    ) -> io::Result<()> {
        let State::Capturing { action, observed } = self.state else {
            return Ok(());
        };
        let observed = observed.saturating_add(1);
        self.total_observed += 1;
        self.state = State::Capturing { action, observed };
        self.mark(capture, "class_c_acceleration_context", Some(context))?;
        if observed < ACTIONS[action].target {
            return Ok(());
        }
        self.mark(capture, "class_c_acceleration_action_finished", None)?;
        self.advance(capture, action)
    }

    fn advance(&mut self, capture: &mut MotionCapture, action: usize) -> io::Result<()> {
        self.clear_context_window();
        let next_action = action + 1;
        if next_action == ACTIONS.len() {
            capture.set_label("class_c_acceleration_experiment_complete")?;
            self.mark(capture, "class_c_acceleration_experiment_finished", None)?;
            self.state = State::Complete;
        } else {
            self.state = State::Positioning {
                action: next_action,
                observed: 0,
            };
            self.set_label(capture, "positioning")?;
        }
        Ok(())
    }

    fn clear_context_window(&mut self) {
        self.previous_normal = None;
        self.pending.clear();
    }

    fn set_label(&self, capture: &mut MotionCapture, stage: &str) -> io::Result<()> {
        let Some(action) = self.action_index() else {
            return Ok(());
        };
        capture.set_label(&format!(
            "class_c_acceleration_{stage}_{}",
            ACTIONS[action].id
        ))
    }

    fn mark(
        &self,
        capture: &mut MotionCapture,
        event: &'static str,
        context: Option<ClassCAccelerationContext>,
    ) -> io::Result<()> {
        let action = self.action_index().unwrap_or(ACTIONS.len() - 1);
        capture.mark_class_c_acceleration_experiment(AccelerationExperimentMarker {
            event,
            action: &ACTIONS[action],
            observed: self.observed_in_action(),
            total_observed: self.total_observed,
            rejected: self.rejected,
            context,
        })
    }

    const fn action_index(&self) -> Option<usize> {
        match self.state {
            State::Positioning { action, .. } | State::Capturing { action, .. } => Some(action),
            State::Idle | State::Complete => None,
        }
    }
}

impl PendingContext {
    fn complete(
        self,
        next: Observation<NormalAccelerationPacket>,
    ) -> Option<ClassCAccelerationContext> {
        let normal_span_ticks = Self::sensor_delta(
            self.previous.packet.sensor_timestamp,
            next.packet.sensor_timestamp,
        );
        let previous_to_focus_sensor_ticks = Self::sensor_delta(
            self.previous.packet.sensor_timestamp,
            self.focus.packet.sensor_timestamp,
        );
        if !(8..=20).contains(&normal_span_ticks)
            || previous_to_focus_sensor_ticks == 0
            || previous_to_focus_sensor_ticks >= normal_span_ticks
        {
            return None;
        }
        Some(ClassCAccelerationContext {
            previous: AccelerationPoint {
                notification_sequence: self.previous.notification_sequence,
                packet: self.previous.packet,
            },
            focus: AccelerationPoint {
                notification_sequence: self.focus.notification_sequence,
                packet: self.focus.packet,
            },
            next: AccelerationPoint {
                notification_sequence: next.notification_sequence,
                packet: next.packet,
            },
            normal_span_ticks,
            first_sample_tick: (normal_span_ticks + 1) / 3,
            second_sample_tick: normal_span_ticks.saturating_mul(2).div_ceil(3),
            previous_to_focus_sensor_ticks,
            focus_to_next_sensor_ticks: Self::sensor_delta(
                self.focus.packet.sensor_timestamp,
                next.packet.sensor_timestamp,
            ),
            previous_to_focus_host_ns: self
                .focus
                .received_at
                .saturating_duration_since(self.previous.received_at)
                .as_nanos(),
            focus_to_next_host_ns: next
                .received_at
                .saturating_duration_since(self.focus.received_at)
                .as_nanos(),
        })
    }

    fn sensor_delta(from: u16, to: u16) -> u16 {
        to.wrapping_sub(from) & 0x0fff
    }
}

struct PackedAccelerationBody<'a> {
    bytes: &'a [u8],
}

impl<'a> PackedAccelerationBody<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    fn read_unsigned(&self, offset: usize, width: usize) -> u32 {
        let mut value = 0_u32;
        for bit in 0..width {
            let source = offset + bit;
            value |= u32::from((self.bytes[source / 8] >> (source & 7)) & 1) << bit;
        }
        value
    }

    fn read_signed(&self, offset: usize, width: usize) -> i32 {
        let shift = u32::BITS as usize - width;
        ((self.read_unsigned(offset, width) << shift) as i32) >> shift
    }

    fn raw_vector(&self, offset: usize, width: usize, stride: usize, shift: u32) -> [i32; 3] {
        std::array::from_fn(|axis| self.read_signed(offset + axis * stride, width) << shift)
    }
}
