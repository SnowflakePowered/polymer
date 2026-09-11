use super::PluginInvocationError;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

pub(super) struct IsolateDeadline {
    completed: std::sync::mpsc::SyncSender<()>,
    expired: Arc<AtomicBool>,
    watchdog: Option<thread::JoinHandle<()>>,
}

impl IsolateDeadline {
    pub(super) fn start(
        isolate: deno_core::v8::IsolateHandle,
        timeout: std::time::Duration,
    ) -> Result<Self, PluginInvocationError> {
        let (completed, receiver) = std::sync::mpsc::sync_channel(1);
        let expired = Arc::new(AtomicBool::new(false));
        let watchdog_expired = expired.clone();
        let watchdog = thread::Builder::new()
            .name("polymer-plugin-deadline".to_owned())
            .spawn(move || {
                if receiver.recv_timeout(timeout).is_err() {
                    watchdog_expired.store(true, Ordering::Release);
                    isolate.terminate_execution();
                }
            })
            .map_err(PluginInvocationError::worker)?;
        Ok(Self {
            completed,
            expired,
            watchdog: Some(watchdog),
        })
    }

    pub(super) fn finish(self, runtime: &mut deno_core::JsRuntime) -> bool {
        let expired = self.expired.clone();
        drop(self);
        let expired = expired.load(Ordering::Acquire);
        if expired {
            runtime.v8_isolate().cancel_terminate_execution();
        }
        expired
    }
}

impl Drop for IsolateDeadline {
    fn drop(&mut self) {
        let _ = self.completed.send(());
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
    }
}
