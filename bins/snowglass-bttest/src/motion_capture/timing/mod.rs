use std::{io, time::Instant};

use serde_json::{Value, json};

use super::{GAMECUBE_INPUT_UUID, MOTION_DATA_OFFSET, MOTION_LENGTH_OFFSET, MotionCapture};

pub mod ui;

const FULL_MOTION_LENGTH: usize = 40;
const NORMAL_TIMING_CLASS: u8 = 0x0e;
const EXTENDED_TIMING_CLASS: u8 = 0x0f;
const CONTEXTS_PER_ACTION: u16 = 16;

const ACTIONS: &[TimingAction] = &[
    TimingAction::new(
        "stationary_warmup",
        "Lay the controller face-up and leave it completely untouched.",
        "Keep it still while the sensor warms and class-F contexts accumulate.",
        false,
    ),
    TimingAction::new(
        "pitch_steady",
        "Hold the controller clear of the desk.",
        "Make repeated smooth pitch rotations at the steadiest speed you can manage.",
        false,
    ),
    TimingAction::new(
        "roll_steady",
        "Hold the controller clear of the desk.",
        "Make repeated smooth roll rotations at the steadiest speed you can manage.",
        false,
    ),
    TimingAction::new(
        "yaw_steady",
        "Hold the controller approximately face-up.",
        "Make repeated smooth yaw rotations at the steadiest speed you can manage.",
        false,
    ),
    TimingAction::new(
        "mixed_flicks",
        "Hold the controller clear of the desk.",
        "Make sharp isolated flicks on all three axes, returning to rest between them.",
        false,
    ),
    TimingAction::new(
        "isolated_taps",
        "Lay the controller face-up on a firm surface.",
        "Tap a grip or shoulder edge once every one or two seconds without changing its pose.",
        false,
    ),
    TimingAction::new(
        "stationary_warmed",
        "After the motion actions, lay the controller face-up again.",
        "Keep it completely untouched to provide a warmed stationary comparison.",
        false,
    ),
    TimingAction::new(
        "stationary_charging",
        "Leave the controller face-up and connect it to USB power if that does not end Bluetooth input.",
        "Keep it still while charging. Skip this action if attaching USB switches transport or disconnects it.",
        true,
    ),
];

#[derive(Clone, Copy)]
pub struct TimingAction {
    pub label: &'static str,
    pub positioning_instruction: &'static str,
    pub capture_instruction: &'static str,
    pub optional: bool,
}

impl TimingAction {
    const fn new(
        label: &'static str,
        positioning_instruction: &'static str,
        capture_instruction: &'static str,
        optional: bool,
    ) -> Self {
        Self {
            label,
            positioning_instruction,
            capture_instruction,
            optional,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimingStage {
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
pub struct TimingPacket {
    pub sensor_timestamp: u16,
    pub elapsed_ticks: u16,
    pub timing_class: u8,
    pub motion_length: u8,
    pub range_flags: Option<u8>,
    pub omitted_component: Option<u8>,
    pub reserved_bit_4: Option<u8>,
    pub bit_287: Option<u8>,
    pub final_body_byte: Option<u8>,
}

impl TimingPacket {
    pub fn parse(characteristic: &str, bytes: &[u8]) -> Option<Self> {
        if characteristic != GAMECUBE_INPUT_UUID {
            return None;
        }
        let motion_length = *bytes.get(MOTION_LENGTH_OFFSET)?;
        let motion = bytes
            .get(MOTION_DATA_OFFSET..MOTION_DATA_OFFSET.checked_add(usize::from(motion_length))?)?;
        let header = u32::from_le_bytes(motion.get(..4)?.try_into().ok()?);
        let body = motion.get(4..)?;
        let timing_class = ((header >> 24) & 0x0f) as u8;
        let full_body = (usize::from(motion_length) == FULL_MOTION_LENGTH).then_some(body);
        let final_body_byte = full_body.and_then(|body| body.get(35)).copied();
        Some(Self {
            sensor_timestamp: (header & 0x0fff) as u16,
            elapsed_ticks: ((header >> 12) & 0x0fff) as u16,
            timing_class,
            motion_length,
            range_flags: full_body
                .and_then(|body| body.first())
                .map(|byte| byte & 0b11),
            omitted_component: full_body
                .and_then(|body| body.first())
                .map(|byte| (byte >> 2) & 0b11),
            reserved_bit_4: full_body
                .and_then(|body| body.first())
                .map(|byte| (byte >> 4) & 1),
            bit_287: (timing_class == EXTENDED_TIMING_CLASS)
                .then(|| final_body_byte.map(|byte| byte >> 7))
                .flatten(),
            final_body_byte,
        })
    }
}

#[derive(Clone, Copy)]
pub struct TimingPoint {
    pub notification_sequence: u64,
    pub packet: TimingPacket,
}

impl TimingPoint {
    pub fn json(self) -> Value {
        json!({
            "notification_sequence": self.notification_sequence,
            "sensor_timestamp": self.packet.sensor_timestamp,
            "elapsed_ticks": self.packet.elapsed_ticks,
            "timing_class": format!("0x{:x}", self.packet.timing_class),
            "motion_length": self.packet.motion_length,
            "range_flags": self.packet.range_flags,
            "omitted_component": self.packet.omitted_component,
            "reserved_bit_4": self.packet.reserved_bit_4,
            "bit_287": self.packet.bit_287,
            "final_body_byte": self.packet.final_body_byte,
        })
    }
}

#[derive(Clone, Copy)]
struct Observation {
    point: TimingPoint,
    received_at: Instant,
}

#[derive(Clone, Copy)]
struct PendingContext {
    previous: Option<Observation>,
    focus: Observation,
    kind: FocusKind,
}

#[derive(Clone, Copy)]
enum FocusKind {
    Extended,
    NormalElapsedOutlier,
}

#[derive(Clone, Copy)]
pub struct TimingContext {
    pub previous: Option<TimingPoint>,
    pub focus: TimingPoint,
    pub next: TimingPoint,
    pub previous_to_focus_host_ns: Option<u128>,
    pub focus_to_next_host_ns: u128,
    pub previous_to_focus_sensor_ticks: Option<u16>,
    pub focus_to_next_sensor_ticks: u16,
}

pub struct TimingExperimentMarker {
    pub event: &'static str,
    pub action: &'static str,
    pub action_number: usize,
    pub action_count: usize,
    pub observed: u16,
    pub target: u16,
    pub bit_287_counts: [u64; 2],
    pub elapsed_outliers: u64,
    pub context: Option<TimingContext>,
}

#[derive(Default)]
pub struct TimingExperiment {
    state: State,
    previous: Option<Observation>,
    pending: Option<PendingContext>,
    total_observed: u64,
    bit_287_counts: [u64; 2],
    elapsed_outliers: u64,
}

impl TimingExperiment {
    pub fn start(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        self.reset();
        self.state = State::Positioning { action: 0 };
        self.set_label(capture, "positioning")?;
        self.mark(capture, "timing_experiment_started", 0, None)
    }

    pub fn begin_action(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        let State::Positioning { action } = self.state else {
            return Ok(());
        };
        self.pending = None;
        self.state = State::Capturing {
            action,
            observed: 0,
        };
        self.set_label(capture, "capturing")?;
        self.mark(capture, "timing_action_started", 0, None)
    }

    pub fn observe(
        &mut self,
        capture: &mut MotionCapture,
        packet: TimingPacket,
        received_at: Instant,
    ) -> io::Result<()> {
        let current = Observation {
            point: TimingPoint {
                notification_sequence: capture.samples(),
                packet,
            },
            received_at,
        };
        if matches!(self.state, State::Capturing { .. }) {
            if let Some(pending) = self.pending.take() {
                let kind = pending.kind;
                let context = Self::context(pending, current);
                if self.record_context(capture, kind, context)? {
                    self.previous = Some(current);
                    return Ok(());
                }
            }
            if let Some(kind) = Self::focus_kind(packet) {
                self.pending = Some(PendingContext {
                    previous: self.previous,
                    focus: current,
                    kind,
                });
            }
        }
        self.previous = Some(current);
        Ok(())
    }

    pub fn skip_action(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        let Some(action) = self.action_index() else {
            return Ok(());
        };
        self.pending = None;
        self.mark(
            capture,
            "timing_action_skipped",
            self.observed_in_action(),
            None,
        )?;
        self.advance(capture, action)
    }

    pub fn restart(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        self.start(capture)
    }

    pub fn abort(&mut self, capture: &mut MotionCapture) -> io::Result<()> {
        if self.is_running() {
            self.mark(
                capture,
                "timing_experiment_aborted",
                self.observed_in_action(),
                None,
            )?;
        }
        self.state = State::Idle;
        self.pending = None;
        Ok(())
    }

    pub fn reset(&mut self) {
        self.state = State::Idle;
        self.previous = None;
        self.pending = None;
        self.total_observed = 0;
        self.bit_287_counts = [0; 2];
        self.elapsed_outliers = 0;
    }

    pub const fn stage(&self) -> TimingStage {
        match self.state {
            State::Idle => TimingStage::Idle,
            State::Positioning { .. } => TimingStage::Positioning,
            State::Capturing { .. } => TimingStage::Capturing,
            State::Complete => TimingStage::Complete,
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

    pub fn current_action(&self) -> Option<&'static TimingAction> {
        self.action_index().map(|action| &ACTIONS[action])
    }

    pub fn action_number(&self) -> Option<usize> {
        self.action_index().map(|action| action + 1)
    }

    pub const fn action_count(&self) -> usize {
        ACTIONS.len()
    }

    pub const fn target_per_action(&self) -> u16 {
        CONTEXTS_PER_ACTION
    }

    pub const fn total_observed(&self) -> u64 {
        self.total_observed
    }

    pub const fn bit_287_counts(&self) -> [u64; 2] {
        self.bit_287_counts
    }

    pub const fn elapsed_outliers(&self) -> u64 {
        self.elapsed_outliers
    }

    pub const fn observed_in_action(&self) -> u16 {
        match self.state {
            State::Capturing { observed, .. } => observed,
            State::Idle | State::Positioning { .. } | State::Complete => 0,
        }
    }

    fn record_context(
        &mut self,
        capture: &mut MotionCapture,
        kind: FocusKind,
        context: TimingContext,
    ) -> io::Result<bool> {
        if matches!(kind, FocusKind::NormalElapsedOutlier) {
            self.elapsed_outliers += 1;
            self.mark(
                capture,
                "timing_normal_elapsed_outlier_context",
                self.observed_in_action(),
                Some(context),
            )?;
            return Ok(false);
        }
        let State::Capturing { action, observed } = self.state else {
            return Ok(false);
        };
        let observed = observed + 1;
        self.total_observed += 1;
        if let Some(bit) = context.focus.packet.bit_287 {
            self.bit_287_counts[usize::from(bit)] += 1;
        }
        self.state = State::Capturing { action, observed };
        self.mark(capture, "timing_class_f_context", observed, Some(context))?;
        if observed < CONTEXTS_PER_ACTION {
            return Ok(false);
        }
        self.mark(capture, "timing_action_finished", observed, None)?;
        self.advance(capture, action)?;
        Ok(true)
    }

    fn advance(&mut self, capture: &mut MotionCapture, action: usize) -> io::Result<()> {
        let next_action = action + 1;
        if next_action == ACTIONS.len() {
            capture.set_label("timing_experiment_complete")?;
            self.mark(capture, "timing_experiment_finished", 0, None)?;
            self.state = State::Complete;
        } else {
            self.state = State::Positioning {
                action: next_action,
            };
            self.set_label(capture, "positioning")?;
        }
        Ok(())
    }

    fn context(pending: PendingContext, next: Observation) -> TimingContext {
        TimingContext {
            previous: pending.previous.map(|observation| observation.point),
            focus: pending.focus.point,
            next: next.point,
            previous_to_focus_host_ns: pending.previous.map(|previous| {
                pending
                    .focus
                    .received_at
                    .saturating_duration_since(previous.received_at)
                    .as_nanos()
            }),
            focus_to_next_host_ns: next
                .received_at
                .saturating_duration_since(pending.focus.received_at)
                .as_nanos(),
            previous_to_focus_sensor_ticks: pending.previous.map(|previous| {
                pending
                    .focus
                    .point
                    .packet
                    .sensor_timestamp
                    .wrapping_sub(previous.point.packet.sensor_timestamp)
                    & 0x0fff
            }),
            focus_to_next_sensor_ticks: next
                .point
                .packet
                .sensor_timestamp
                .wrapping_sub(pending.focus.point.packet.sensor_timestamp)
                & 0x0fff,
        }
    }

    fn focus_kind(packet: TimingPacket) -> Option<FocusKind> {
        if packet.timing_class == EXTENDED_TIMING_CLASS {
            Some(FocusKind::Extended)
        } else if packet.timing_class == NORMAL_TIMING_CLASS
            && !(11..=13).contains(&packet.elapsed_ticks)
        {
            Some(FocusKind::NormalElapsedOutlier)
        } else {
            None
        }
    }

    fn set_label(&self, capture: &mut MotionCapture, stage: &str) -> io::Result<()> {
        let Some(action) = self.action_index() else {
            return Ok(());
        };
        capture.set_label(&format!("timing_{stage}_{}", ACTIONS[action].label))
    }

    fn mark(
        &self,
        capture: &mut MotionCapture,
        event: &'static str,
        observed: u16,
        context: Option<TimingContext>,
    ) -> io::Result<()> {
        let action = self.action_index().unwrap_or(ACTIONS.len() - 1);
        capture.mark_timing_experiment(TimingExperimentMarker {
            event,
            action: ACTIONS[action].label,
            action_number: action + 1,
            action_count: ACTIONS.len(),
            observed,
            target: CONTEXTS_PER_ACTION,
            bit_287_counts: self.bit_287_counts,
            elapsed_outliers: self.elapsed_outliers,
            context,
        })
    }

    const fn action_index(&self) -> Option<usize> {
        match self.state {
            State::Positioning { action } | State::Capturing { action, .. } => Some(action),
            State::Idle | State::Complete => None,
        }
    }
}
