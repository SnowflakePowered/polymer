mod admission;
mod state;

use std::sync::{Arc, mpsc};
use std::time::Duration;

use parking_lot::Mutex;

use crate::Error;
use admission::Admission;
pub(super) use admission::Mutation;
use state::Writeback;

#[derive(Clone)]
pub(super) struct ReconcileWorker {
    commands: mpsc::Sender<ReconcileCommand>,
    admission: Arc<Admission>,
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
        let admission = Arc::new(Admission::default());
        let background = admission.clone();
        let thread = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || Self::run(pending, background, reconcile))
            .map_err(|source| Error::Io {
                path: name.to_owned(),
                source,
            })?;
        Ok(Self {
            commands,
            admission,
            thread: Arc::new(Mutex::new(Some(thread))),
        })
    }

    pub(super) fn begin(&self, schedule: bool) -> Result<Mutation<'_>, Error> {
        self.admission.begin(self, schedule)
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
        let mut thread = self.thread.lock();
        if thread.is_none() {
            return Ok(());
        }
        self.admission.close();
        let (finished, result) = mpsc::sync_channel(1);
        self.commands
            .send(ReconcileCommand::Shutdown(finished))
            .map_err(|_| Error::Unsupported("writeback worker stopped".to_owned()))?;
        result
            .recv()
            .map_err(|_| Error::Unsupported("writeback worker stopped".to_owned()))??;
        thread
            .take()
            .expect("the shutdown caller owns the worker handle")
            .join()
            .map_err(|_| Error::Unsupported("writeback worker panicked".to_owned()))
    }

    fn run(
        commands: mpsc::Receiver<ReconcileCommand>,
        admission: Arc<Admission>,
        reconcile: impl Fn() -> Result<(), Error>,
    ) {
        let mut writeback = Writeback::default();
        loop {
            match writeback.receive(&commands) {
                Ok(ReconcileCommand::Dirty) => {
                    admission.consume_dirty();
                    writeback.mark_dirty();
                }
                Ok(ReconcileCommand::Sync(finished)) => {
                    let _ = finished.send(writeback.reconcile(&reconcile));
                }
                Ok(ReconcileCommand::Shutdown(finished)) => {
                    let result = writeback.reconcile(&reconcile);
                    let stop = result.is_ok();
                    let _ = finished.send(result);
                    if stop {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let _ = writeback.reconcile(&reconcile);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    while writeback.dirty && writeback.reconcile(&reconcile).is_err() {
                        std::thread::sleep(WRITEBACK_MAX);
                    }
                    break;
                }
            }
        }
    }
}
