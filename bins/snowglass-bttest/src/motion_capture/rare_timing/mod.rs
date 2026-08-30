use std::{collections::VecDeque, io, time::Instant};

use super::{MotionCapture, timing::TimingPacket, timing::TimingPoint};

pub mod ui;

pub const TARGET_ELAPSED_TICKS: [u16; 10] = [15, 22, 23, 25, 26, 35, 36, 37, 47, 48];
pub const TARGET_CONTEXTS_PER_AXIS: u16 = 10;

const FULL_MOTION_LENGTH: u8 = 40;
const NORMAL_TIMING_CLASS: u8 = 0x0e;
const EXTENDED_TIMING_CLASS: u8 = 0x0f;
const CONTINUATION_TIMING_CLASS: u8 = 0x0d;
const CONTINUATION_ELAPSED_TICKS: u16 = 9;
const MAX_CONTEXT_PACKETS: usize = 24;
const MAX_CONTEXT_SENSOR_TICKS: u16 = 96;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MotionAxis {
    Pitch,
    Roll,
    Yaw,
}

impl MotionAxis {
    pub const ALL: [Self; 3] = [Self::Pitch, Self::Roll, Self::Yaw];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Pitch => "pitch",
            Self::Roll => "roll",
            Self::Yaw => "yaw",
        }
    }

    pub const fn instruction(self) -> &'static str {
        match self {
            Self::Pitch => "Make one long, smooth pitch arc in one direction at a steady speed.",
            Self::Roll => "Make one long, smooth roll arc in one direction at a steady speed.",
            Self::Yaw => "Make one long, smooth yaw arc in one direction at a steady speed.",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Pitch => 0,
            Self::Roll => 1,
            Self::Yaw => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RareTimingStage {
    Idle,
    Positioning,
    Capturing,
    Complete,
}

#[derive(Clone, Copy, Debug, Default)]
enum State {
    #[default]
    Idle,
    Positioning(MotionAxis),
    Capturing(MotionAxis),
    Complete,
}

#[derive(Clone, Copy)]
struct Observation {
    point: TimingPoint,
    received_at: Instant,
}

#[derive(Clone)]
struct PendingContext {
    axis: MotionAxis,
    before: [Observation; 2],
    bridge_before: Vec<Observation>,
    focus: Observation,
    after: Vec<Observation>,
    bridge_after: Vec<Observation>,
    timeline: Vec<Observation>,
    continuation: Option<Observation>,
}

pub struct RareTimingContext {
    pub before: [TimingPoint; 2],
    pub bridge_before: Vec<TimingPoint>,
    pub focus: TimingPoint,
    pub after: [TimingPoint; 2],
    pub bridge_after: Vec<TimingPoint>,
    pub continuation: Option<TimingPoint>,
    pub timeline: Vec<TimingPoint>,
    pub host_deltas_ns: Vec<u128>,
    pub sensor_deltas_ticks: Vec<u16>,
}

enum PendingAdvance {
    Waiting(PendingContext),
    Complete(MotionAxis, RareTimingContext),
    Rejected,
}

pub struct RareTimingExperimentMarker {
    pub event: &'static str,
    pub axis: Option<MotionAxis>,
    pub completed_contexts: u64,
    pub rejected_contexts: u64,
    pub observed_candidates: [[u64; TARGET_ELAPSED_TICKS.len()]; MotionAxis::ALL.len()],
    pub completed_by_axis: [[u16; TARGET_ELAPSED_TICKS.len()]; MotionAxis::ALL.len()],
    pub context: Option<RareTimingContext>,
}

#[derive(Default)]
pub struct RareTimingExperiment {
    state: State,
    history: VecDeque<Observation>,
    pending: VecDeque<PendingContext>,
    observed_candidates: [[u64; TARGET_ELAPSED_TICKS.len()]; MotionAxis::ALL.len()],
    completed_by_axis: [[u16; TARGET_ELAPSED_TICKS.len()]; MotionAxis::ALL.len()],
    completed_contexts: u64,
    rejected_contexts: u64,
}

impl RareTimingExperiment {
    pub fn start(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        self.reset();
        self.state = State::Positioning(MotionAxis::Pitch);
        self.set_label(capture, "positioning", MotionAxis::Pitch)?;
        self.mark(capture, "rare_timing_experiment_started", None)
    }

    pub fn position_axis(
        &mut self,
        capture: &mut MotionCapture,
        axis: MotionAxis,
    ) -> io::Result<()> {
        if matches!(self.state, State::Idle | State::Complete) {
            return Ok(());
        }
        self.clear_context_window();
        self.state = State::Positioning(axis);
        self.set_label(capture, "positioning", axis)?;
        self.mark(capture, "rare_timing_axis_positioning", None)
    }

    pub fn begin_axis(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        let State::Positioning(axis) = self.state else {
            return Ok(());
        };
        self.clear_context_window();
        self.state = State::Capturing(axis);
        self.set_label(capture, "capturing", axis)?;
        self.mark(capture, "rare_timing_axis_started", None)
    }

    pub fn observe(
        &mut self,
        capture: &mut MotionCapture,
        packet: TimingPacket,
        received_at: Instant,
    ) -> io::Result<()> {
        let State::Capturing(_) = self.state else {
            return Ok(());
        };
        let current = Observation {
            point: TimingPoint {
                notification_sequence: capture.samples(),
                packet,
            },
            received_at,
        };

        self.advance_pending(capture, current)?;
        let State::Capturing(axis) = self.state else {
            return Ok(());
        };
        if let Some(bucket) = Self::target_bucket(packet) {
            self.observed_candidates[axis.index()][bucket] += 1;
            if let Some(pending) = PendingContext::from_history(axis, &self.history, current) {
                self.pending.push_back(pending);
            } else {
                self.rejected_contexts += 1;
            }
        }
        self.history.push_back(current);
        if self.history.len() > MAX_CONTEXT_PACKETS {
            self.history.pop_front();
        }
        Ok(())
    }

    pub fn restart(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        self.start(capture)
    }

    pub fn abort(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        if self.is_running() {
            self.mark(capture, "rare_timing_experiment_aborted", None)?;
        }
        self.state = State::Idle;
        self.clear_context_window();
        Ok(())
    }

    pub fn finish(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        if self.is_engaged() {
            self.mark(capture, "rare_timing_experiment_summary", None)?;
        }
        self.state = State::Idle;
        self.clear_context_window();
        Ok(())
    }

    pub fn reset(&mut self) {
        self.state = State::Idle;
        self.clear_context_window();
        self.observed_candidates = [[0; TARGET_ELAPSED_TICKS.len()]; MotionAxis::ALL.len()];
        self.completed_by_axis = [[0; TARGET_ELAPSED_TICKS.len()]; MotionAxis::ALL.len()];
        self.completed_contexts = 0;
        self.rejected_contexts = 0;
    }

    pub const fn stage(&self) -> RareTimingStage {
        match self.state {
            State::Idle => RareTimingStage::Idle,
            State::Positioning(_) => RareTimingStage::Positioning,
            State::Capturing(_) => RareTimingStage::Capturing,
            State::Complete => RareTimingStage::Complete,
        }
    }

    pub const fn current_axis(&self) -> Option<MotionAxis> {
        match self.state {
            State::Positioning(axis) | State::Capturing(axis) => Some(axis),
            State::Idle | State::Complete => None,
        }
    }

    pub const fn is_engaged(&self) -> bool {
        !matches!(self.state, State::Idle)
    }

    pub const fn is_running(&self) -> bool {
        matches!(self.state, State::Positioning(_) | State::Capturing(_))
    }

    pub const fn completed_contexts(&self) -> u64 {
        self.completed_contexts
    }

    pub const fn rejected_contexts(&self) -> u64 {
        self.rejected_contexts
    }

    pub const fn observed_candidates(
        &self,
    ) -> [[u64; TARGET_ELAPSED_TICKS.len()]; MotionAxis::ALL.len()] {
        self.observed_candidates
    }

    pub const fn completed_by_axis(
        &self,
    ) -> [[u16; TARGET_ELAPSED_TICKS.len()]; MotionAxis::ALL.len()] {
        self.completed_by_axis
    }

    pub const fn target_contexts(&self) -> u64 {
        (TARGET_CONTEXTS_PER_AXIS as u64)
            * (TARGET_ELAPSED_TICKS.len() as u64)
            * (MotionAxis::ALL.len() as u64)
    }

    fn advance_pending(
        &mut self,
        capture: &mut MotionCapture,
        current: Observation,
    ) -> io::Result<()> {
        let mut waiting = VecDeque::new();
        let mut complete = Vec::new();
        for pending in std::mem::take(&mut self.pending) {
            match pending.advance(current) {
                PendingAdvance::Waiting(pending) => waiting.push_back(pending),
                PendingAdvance::Complete(axis, context) => complete.push((axis, context)),
                PendingAdvance::Rejected => self.rejected_contexts += 1,
            }
        }
        self.pending = waiting;
        for (axis, context) in complete {
            self.record_context(capture, axis, context)?;
            if matches!(self.state, State::Complete) {
                break;
            }
        }
        Ok(())
    }

    fn record_context(
        &mut self,
        capture: &mut MotionCapture,
        axis: MotionAxis,
        context: RareTimingContext,
    ) -> io::Result<()> {
        let Some(bucket) = Self::target_bucket(context.focus.packet) else {
            return Ok(());
        };
        self.completed_by_axis[axis.index()][bucket] =
            self.completed_by_axis[axis.index()][bucket].saturating_add(1);
        self.completed_contexts += 1;
        self.mark(capture, "rare_timing_context", Some(context))?;
        if self.quotas_complete() {
            capture.set_label("rare_timing_experiment_complete")?;
            self.mark(capture, "rare_timing_experiment_finished", None)?;
            self.state = State::Complete;
            self.clear_context_window();
        }
        Ok(())
    }

    fn is_normal(packet: TimingPacket) -> bool {
        packet.motion_length == FULL_MOTION_LENGTH && packet.timing_class == NORMAL_TIMING_CLASS
    }

    fn target_bucket(packet: TimingPacket) -> Option<usize> {
        (packet.motion_length == FULL_MOTION_LENGTH && packet.timing_class == EXTENDED_TIMING_CLASS)
            .then(|| {
                TARGET_ELAPSED_TICKS
                    .iter()
                    .position(|elapsed| *elapsed == packet.elapsed_ticks)
            })
            .flatten()
    }

    fn quotas_complete(&self) -> bool {
        self.completed_by_axis
            .iter()
            .flatten()
            .all(|count| *count >= TARGET_CONTEXTS_PER_AXIS)
    }

    fn clear_context_window(&mut self) {
        self.history.clear();
        self.pending.clear();
    }

    fn set_label(
        &self,
        capture: &mut MotionCapture,
        stage: &str,
        axis: MotionAxis,
    ) -> io::Result<()> {
        capture.set_label(&format!("rare_timing_{stage}_{}", axis.label()))
    }

    fn mark(
        &self,
        capture: &mut MotionCapture,
        event: &'static str,
        context: Option<RareTimingContext>,
    ) -> io::Result<()> {
        capture.mark_rare_timing_experiment(RareTimingExperimentMarker {
            event,
            axis: self.current_axis(),
            completed_contexts: self.completed_contexts,
            rejected_contexts: self.rejected_contexts,
            observed_candidates: self.observed_candidates,
            completed_by_axis: self.completed_by_axis,
            context,
        })
    }
}

impl PendingContext {
    fn from_history(
        axis: MotionAxis,
        history: &VecDeque<Observation>,
        focus: Observation,
    ) -> Option<Self> {
        let mut anchor_indices = history
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, observation)| RareTimingExperiment::is_normal(observation.point.packet))
            .take(2)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if anchor_indices.len() != 2 {
            return None;
        }
        anchor_indices.reverse();
        let older = anchor_indices[0];
        let newer = anchor_indices[1];
        let before = [history[older], history[newer]];
        if Self::sensor_delta(before[0], focus) > MAX_CONTEXT_SENSOR_TICKS {
            return None;
        }
        let bridge_before = history
            .iter()
            .enumerate()
            .skip(older)
            .filter(|(index, _)| *index != older && *index != newer)
            .map(|(_, observation)| *observation)
            .collect();
        let mut timeline = history.iter().skip(older).copied().collect::<Vec<_>>();
        timeline.push(focus);
        Some(Self {
            axis,
            before,
            bridge_before,
            focus,
            after: Vec::with_capacity(2),
            bridge_after: Vec::new(),
            timeline,
            continuation: None,
        })
    }

    fn advance(mut self, current: Observation) -> PendingAdvance {
        if Self::sensor_delta(self.focus, current) > MAX_CONTEXT_SENSOR_TICKS
            || self.timeline.len() >= MAX_CONTEXT_PACKETS
        {
            return PendingAdvance::Rejected;
        }
        if self.requires_continuation()
            && self.continuation.is_none()
            && let Some(valid) = self.validate_first_full_continuation(current)
        {
            if !valid {
                return PendingAdvance::Rejected;
            }
            self.continuation = Some(current);
        }
        self.timeline.push(current);
        if RareTimingExperiment::is_normal(current.point.packet) {
            self.after.push(current);
        } else {
            self.bridge_after.push(current);
        }
        if self.after.len() < 2 {
            return PendingAdvance::Waiting(self);
        }
        if self.requires_continuation() && self.continuation.is_none() {
            return PendingAdvance::Rejected;
        }
        let axis = self.axis;
        PendingAdvance::Complete(axis, self.context())
    }

    fn context(self) -> RareTimingContext {
        let host_deltas_ns = self
            .timeline
            .windows(2)
            .map(|pair| {
                pair[1]
                    .received_at
                    .saturating_duration_since(pair[0].received_at)
                    .as_nanos()
            })
            .collect();
        let sensor_deltas_ticks = self
            .timeline
            .windows(2)
            .map(|pair| Self::sensor_delta(pair[0], pair[1]))
            .collect();
        RareTimingContext {
            before: self.before.map(|observation| observation.point),
            bridge_before: self
                .bridge_before
                .into_iter()
                .map(|observation| observation.point)
                .collect(),
            focus: self.focus.point,
            after: [self.after[0].point, self.after[1].point],
            bridge_after: self
                .bridge_after
                .into_iter()
                .map(|observation| observation.point)
                .collect(),
            continuation: self.continuation.map(|observation| observation.point),
            timeline: self
                .timeline
                .into_iter()
                .map(|observation| observation.point)
                .collect(),
            host_deltas_ns,
            sensor_deltas_ticks,
        }
    }

    fn validate_first_full_continuation(&self, current: Observation) -> Option<bool> {
        (current.point.packet.motion_length == FULL_MOTION_LENGTH).then_some(
            current.point.packet.timing_class == CONTINUATION_TIMING_CLASS
                && current.point.packet.elapsed_ticks == CONTINUATION_ELAPSED_TICKS,
        )
    }

    fn requires_continuation(&self) -> bool {
        self.focus.point.packet.elapsed_ticks == 15
    }

    fn sensor_delta(from: Observation, to: Observation) -> u16 {
        to.point
            .packet
            .sensor_timestamp
            .wrapping_sub(from.point.packet.sensor_timestamp)
            & 0x0fff
    }
}
