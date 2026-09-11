use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use tokio::sync::Notify;

use crate::{Admission, ReplyClosed};

struct State {
    // Running -> Closing -> Closed. Failed is a terminal abnormal exit.
    stage: AtomicU8,
    changed: Notify,
    admission: Admission,
    cancel: Option<Box<dyn Fn() + Send + Sync>>,
}

#[derive(Clone)]
pub struct ExecutorLifetime(Arc<State>);

impl Default for ExecutorLifetime {
    fn default() -> Self {
        Self::new(Admission::default(), None)
    }
}

impl ExecutorLifetime {
    pub fn new(admission: Admission, cancel: Option<Box<dyn Fn() + Send + Sync>>) -> Self {
        Self(Arc::new(State {
            stage: AtomicU8::new(0),
            changed: Notify::new(),
            admission,
            cancel,
        }))
    }

    pub fn admission(&self) -> Admission {
        self.0.admission.clone()
    }

    pub fn is_running(&self) -> bool {
        self.0.stage.load(Ordering::Acquire) == 0
    }

    pub fn begin_close(&self) {
        if self
            .0
            .stage
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.0.admission.close();
            if let Some(cancel) = &self.0.cancel {
                cancel();
            }
        }
    }

    pub fn finish(&self, failed: bool) {
        self.0.admission.close();
        self.0
            .stage
            .fetch_max(if failed { 3 } else { 2 }, Ordering::AcqRel);
        self.0.changed.notify_waiters();
    }

    /// Run inside the owning thread. Publish completion after local state drops,
    /// including panic unwinding, without another runtime or joining on the GUI.
    pub fn run(self, work: impl FnOnce()) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
        self.finish(result.is_err());
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    pub async fn wait_closed(&self) -> Result<(), ReplyClosed> {
        loop {
            let changed = self.0.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            match self.0.stage.load(Ordering::Acquire) {
                2 => return Ok(()),
                3 => return Err(ReplyClosed),
                _ => changed.await,
            }
        }
    }
}

/// Explicit worker ownership. Sending clients never keep this guard alive.
pub struct WorkerOwner<C> {
    client: C,
    lifetime: ExecutorLifetime,
    stop: Box<dyn Fn() + Send + Sync>,
}

impl<C> WorkerOwner<C> {
    pub fn new(
        client: C,
        lifetime: ExecutorLifetime,
        stop: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self {
            client,
            lifetime,
            stop: Box::new(stop),
        }
    }

    pub fn close(&self) {
        if self.lifetime.is_running() {
            self.lifetime.begin_close();
            (self.stop)();
        }
    }

    pub async fn shutdown(&self) -> Result<(), ReplyClosed> {
        self.close();
        self.lifetime.wait_closed().await
    }
}

impl<C> Drop for WorkerOwner<C> {
    fn drop(&mut self) {
        self.close();
    }
}

impl<C> std::fmt::Debug for WorkerOwner<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WorkerOwner")
    }
}

impl<C> std::ops::Deref for WorkerOwner<C> {
    type Target = C;
    fn deref(&self) -> &C {
        &self.client
    }
}

impl<C: Clone> WorkerOwner<C> {
    pub fn client(&self) -> C {
        self.client.clone()
    }
}
