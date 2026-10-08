use std::sync::mpsc;
use std::time::Instant;

use super::{ReconcileCommand, WRITEBACK_MAX, WRITEBACK_QUIET};
use crate::Error;

pub(super) struct Writeback {
    pub(super) dirty: bool,
    first_dirty: Instant,
    last_dirty: Instant,
}

impl Default for Writeback {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            dirty: false,
            first_dirty: now,
            last_dirty: now,
        }
    }
}

impl Writeback {
    pub(super) fn receive(
        &self,
        commands: &mpsc::Receiver<ReconcileCommand>,
    ) -> Result<ReconcileCommand, mpsc::RecvTimeoutError> {
        if self.dirty {
            let deadline =
                (self.last_dirty + WRITEBACK_QUIET).min(self.first_dirty + WRITEBACK_MAX);
            commands.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        } else {
            commands
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        }
    }

    pub(super) fn mark_dirty(&mut self) {
        let now = Instant::now();
        if !self.dirty {
            self.first_dirty = now;
            self.dirty = true;
        }
        self.last_dirty = now;
    }

    pub(super) fn reconcile(
        &mut self,
        reconcile: &impl Fn() -> Result<(), Error>,
    ) -> Result<(), Error> {
        let result = reconcile();
        self.dirty = result.is_err();
        self.first_dirty = Instant::now();
        self.last_dirty = self.first_dirty;
        result
    }
}
