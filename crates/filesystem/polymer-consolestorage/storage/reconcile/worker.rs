use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::Error;

#[derive(Clone)]
pub(super) struct ReconcileWorker {
    commands: mpsc::Sender<ReconcileCommand>,
    dirty_enqueued: Arc<AtomicBool>,
    thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,
}

enum ReconcileCommand {
    Dirty,
    Sync(mpsc::SyncSender<Result<(), Error>>),
    Shutdown(mpsc::SyncSender<Result<(), Error>>),
}

const WRITEBACK_QUIET: Duration = Duration::from_millis(250);
const WRITEBACK_MAX: Duration = Duration::from_secs(2);

impl ReconcileWorker {
    pub(super) fn spawn(
        name: &str,
        reconcile: impl Fn() -> Result<(), Error> + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let (commands, pending) = mpsc::channel();
        let dirty_enqueued = Arc::new(AtomicBool::new(false));
        let background_enqueued = dirty_enqueued.clone();
        let thread = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || Self::run(pending, background_enqueued, reconcile))
            .map_err(|source| Error::Io {
                path: name.to_owned(),
                source,
            })?;
        Ok(Self {
            commands,
            dirty_enqueued,
            thread: Arc::new(Mutex::new(Some(thread))),
        })
    }

    pub(super) fn schedule(&self) {
        if !self.dirty_enqueued.swap(true, Ordering::AcqRel) {
            let _ = self.commands.send(ReconcileCommand::Dirty);
        }
    }

    pub(super) fn reconcile(&self) -> Result<(), Error> {
        let (finished, result) = mpsc::sync_channel(1);
        self.commands
            .send(ReconcileCommand::Sync(finished))
            .map_err(|_| Error::Unsupported("writeback worker stopped".to_owned()))?;
        result
            .recv()
            .map_err(|_| Error::Unsupported("writeback worker stopped".to_owned()))?
    }

    pub(super) fn shutdown(&self) -> Result<(), Error> {
        let Some(thread) = self.thread.lock().take() else {
            return Ok(());
        };
        let (finished, result) = mpsc::sync_channel(1);
        let sent = self.commands.send(ReconcileCommand::Shutdown(finished));
        let result = match sent {
            Ok(()) => result
                .recv()
                .map_err(|_| Error::Unsupported("writeback worker stopped".to_owned()))?,
            Err(_) => Err(Error::Unsupported("writeback worker stopped".to_owned())),
        };
        if thread.join().is_err() {
            return Err(Error::Unsupported("writeback worker panicked".to_owned()));
        }
        result
    }

    fn run(
        commands: mpsc::Receiver<ReconcileCommand>,
        dirty_enqueued: Arc<AtomicBool>,
        reconcile: impl Fn() -> Result<(), Error>,
    ) {
        let mut dirty = false;
        let mut first_dirty = Instant::now();
        let mut last_dirty = first_dirty;
        loop {
            let command = if dirty {
                let deadline = (last_dirty + WRITEBACK_QUIET).min(first_dirty + WRITEBACK_MAX);
                commands.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            } else {
                match commands.recv() {
                    Ok(command) => Ok(command),
                    Err(_) => break,
                }
            };
            match command {
                Ok(ReconcileCommand::Dirty) => {
                    dirty_enqueued.store(false, Ordering::Release);
                    let now = Instant::now();
                    if !dirty {
                        first_dirty = now;
                        dirty = true;
                    }
                    last_dirty = now;
                }
                Ok(ReconcileCommand::Sync(finished)) => {
                    let result = if dirty { reconcile() } else { Ok(()) };
                    if result.is_ok() {
                        dirty = false;
                    } else {
                        first_dirty = Instant::now();
                        last_dirty = first_dirty;
                    }
                    let _ = finished.send(result);
                }
                Ok(ReconcileCommand::Shutdown(finished)) => {
                    let result = if dirty { reconcile() } else { Ok(()) };
                    let _ = finished.send(result);
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if reconcile().is_ok() {
                        dirty = false;
                    } else {
                        first_dirty = Instant::now();
                        last_dirty = first_dirty;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if dirty {
                        let _ = reconcile();
                    }
                    break;
                }
            }
        }
    }
}
