//! Input the game may receive, held until script decides whether it claims it.

use crate::shell::protocol::InputOutcome;
use polymer_protocol::input::PointerPhase;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// How long a held event waits for script before it is released as
/// unclaimed, so a stalled or paused shell cannot withhold game input.
const HOLD_TIMEOUT: Duration = Duration::from_millis(100);

pub(super) enum HeldInput {
    /// A key pressed while the game owned keyboard input.
    Key {
        physical_key: Option<String>,
        pressed: bool,
    },
    /// A pointer event, with the shell button state it changed so an
    /// unclaimed press or release can be reverted.
    Pointer {
        phase: PointerPhase,
        button: u32,
        flag: u8,
        flag_was_set: bool,
    },
    Wheel {
        horizontal: i32,
        vertical: i32,
    },
}

struct Held {
    sequence: u64,
    arrived: Instant,
    input: HeldInput,
    outcome: Option<InputOutcome>,
}

/// Releases held input strictly in arrival order, so a key or button release
/// never reaches the game ahead of its press.
#[derive(Default)]
pub(super) struct InputHold {
    next_sequence: u64,
    queue: VecDeque<Held>,
}

impl InputHold {
    pub(super) fn hold(&mut self, input: HeldInput) -> u64 {
        self.next_sequence += 1;
        self.queue.push_back(Held {
            sequence: self.next_sequence,
            arrived: Instant::now(),
            input,
            outcome: None,
        });
        self.next_sequence
    }

    /// Records script's decision. Outcomes for events already released by
    /// the timeout are ignored.
    pub(super) fn settle(&mut self, sequence: u64, outcome: InputOutcome) {
        if let Some(held) = self.queue.iter_mut().find(|held| held.sequence == sequence) {
            held.outcome = Some(outcome);
        }
    }

    /// Takes every event at the front of the queue that has been settled or
    /// has waited longer than `HOLD_TIMEOUT`. A timed-out event counts as
    /// unclaimed and not over a game surface.
    pub(super) fn release(&mut self, now: Instant) -> Vec<(HeldInput, InputOutcome)> {
        let mut released = Vec::new();
        while let Some(front) = self.queue.front() {
            let expired = now.saturating_duration_since(front.arrived) >= HOLD_TIMEOUT;
            if front.outcome.is_none() && !expired {
                break;
            }
            let held = self.queue.pop_front().expect("front exists");
            if held.outcome.is_none() {
                tracing::warn!(
                    sequence = held.sequence,
                    "polymer: shell did not settle held input in time"
                );
            }
            released.push((held.input, held.outcome.unwrap_or_default()));
        }
        released
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.queue.front().map(|held| held.arrived + HOLD_TIMEOUT)
    }
}
