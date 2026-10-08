use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};

use super::{FinalizationReason, ResourceLease};

static RETRIES: FinalizationRetry = FinalizationRetry {
    state: Mutex::new(Pending {
        leases: Vec::new(),
        running: false,
    }),
    changed: Condvar::new(),
};

pub(super) struct FinalizationRetry {
    state: Mutex<Pending>,
    changed: Condvar,
}

struct Pending {
    leases: Vec<Arc<dyn ResourceLease>>,
    running: bool,
}

impl FinalizationRetry {
    pub(super) fn retain(lease: Arc<dyn ResourceLease>) {
        let mut state = RETRIES.state.lock();
        state.leases.push(lease);
        RETRIES.changed.notify_one();
        if !state.running {
            match std::thread::Builder::new()
                .name("polymer-storage-finalization".to_owned())
                .spawn(|| RETRIES.run())
            {
                Ok(_) => state.running = true,
                Err(error) => {
                    tracing::error!(%error, "failed to start storage finalization retry; resources remain retained");
                }
            }
        }
    }

    fn run(&self) {
        loop {
            let leases = {
                let mut state = self.state.lock();
                while state.leases.is_empty() {
                    self.changed.wait(&mut state);
                }
                std::mem::take(&mut state.leases)
            };
            let pending = leases
                .into_iter()
                .filter(|lease| lease.finalize(FinalizationReason::Cancelled).is_err())
                .collect::<Vec<_>>();
            self.state.lock().leases.extend(pending);
            std::thread::sleep(Duration::from_secs(2));
        }
    }
}
