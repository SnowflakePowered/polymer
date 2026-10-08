use parking_lot::{Condvar, Mutex};

use super::{ReconcileCommand, ReconcileWorker};
use crate::Error;

#[derive(Default)]
pub(super) struct Admission {
    state: Mutex<State>,
    drained: Condvar,
}

#[derive(Default)]
struct State {
    closing: bool,
    active: usize,
    dirty_enqueued: bool,
}

pub(in crate::storage::reconcile) struct Mutation<'a> {
    worker: &'a ReconcileWorker,
    schedule: bool,
}

impl Admission {
    pub(super) fn begin<'a>(
        &self,
        worker: &'a ReconcileWorker,
        schedule: bool,
    ) -> Result<Mutation<'a>, Error> {
        let mut state = self.state.lock();
        if state.closing {
            return Err(Error::Unsupported(
                "writeback storage is closing".to_owned(),
            ));
        }
        state.active += 1;
        Ok(Mutation { worker, schedule })
    }

    pub(super) fn close(&self) {
        let mut state = self.state.lock();
        state.closing = true;
        while state.active != 0 {
            self.drained.wait(&mut state);
        }
    }

    pub(super) fn consume_dirty(&self) {
        self.state.lock().dirty_enqueued = false;
    }

    fn schedule(&self, worker: &ReconcileWorker) -> Result<(), Error> {
        let mut state = self.state.lock();
        if !state.dirty_enqueued {
            worker
                .commands
                .send(ReconcileCommand::Dirty)
                .map_err(|_| Error::Unsupported("writeback worker stopped".to_owned()))?;
            state.dirty_enqueued = true;
        }
        Ok(())
    }
}

impl Mutation<'_> {
    pub(in crate::storage::reconcile) fn complete(mut self) -> Result<(), Error> {
        if std::mem::take(&mut self.schedule) {
            self.worker.admission.schedule(self.worker)?;
        }
        Ok(())
    }
}

impl Drop for Mutation<'_> {
    fn drop(&mut self) {
        if self.schedule {
            let _ = self.worker.admission.schedule(self.worker);
        }
        let mut state = self.worker.admission.state.lock();
        state.active -= 1;
        if state.active == 0 {
            self.worker.admission.drained.notify_all();
        }
    }
}
