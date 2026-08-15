//! Lightweight event-loop timing shown in the diagnostics overlay.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TurnPhase {
    Capture,
    Present,
    Background,
    Input,
    Overlay,
}

impl TurnPhase {
    const ALL: [Self; 5] = [
        Self::Capture,
        Self::Present,
        Self::Background,
        Self::Input,
        Self::Overlay,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Present => "present",
            Self::Background => "background",
            Self::Input => "input",
            Self::Overlay => "overlay",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct PhaseEntry {
    started: Instant,
    outer_nested: Duration,
}

#[derive(Debug, Default)]
pub(crate) struct TurnProfile {
    spans: [Duration; TurnPhase::ALL.len()],
    nested: Duration,
    turns: u64,
    started: Option<Instant>,
    report: String,
}

impl TurnProfile {
    /// Opens a phase while preserving any time already attributed to children
    /// of its enclosing phase.
    pub(crate) fn enter(&mut self) -> PhaseEntry {
        PhaseEntry {
            started: Instant::now(),
            outer_nested: std::mem::take(&mut self.nested),
        }
    }

    /// Records the phase's exclusive time, subtracting nested measured work.
    pub(crate) fn record(&mut self, phase: TurnPhase, entry: PhaseEntry) {
        let total = entry.started.elapsed();
        self.spans[phase.index()] += total.saturating_sub(self.nested);
        self.nested = entry.outer_nested + total;
    }

    /// Closes a turn and refreshes the report after each sampling interval.
    pub(crate) fn finish_turn(&mut self, now: Instant) {
        self.turns += 1;
        self.nested = Duration::ZERO;
        let started = *self.started.get_or_insert(now);
        let elapsed = now.saturating_duration_since(started);
        if elapsed < snowglass_statistics::SAMPLE_INTERVAL {
            return;
        }
        let seconds = elapsed.as_secs_f64();
        let mut phases = TurnPhase::ALL;
        phases.sort_by(|left, right| self.spans[right.index()].cmp(&self.spans[left.index()]));
        let costs = phases
            .iter()
            .map(|phase| {
                format!(
                    "{} {:.0}",
                    phase.label(),
                    self.spans[phase.index()].as_secs_f64() * 1000.0 / seconds
                )
            })
            .collect::<Vec<_>>()
            .join(" · ");
        self.report = format!(
            "Loop {:.1} turns/s · {costs} ms/s",
            self.turns as f64 / seconds
        );
        self.spans = <[Duration; TurnPhase::ALL.len()]>::default();
        self.turns = 0;
        self.started = Some(now);
    }

    pub(crate) fn report(&self) -> &str {
        &self.report
    }
}
