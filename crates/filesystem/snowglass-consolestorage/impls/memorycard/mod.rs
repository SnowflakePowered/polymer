use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::Error;

mod raw;

pub(crate) use raw::MemoryCardStorage;

#[derive(Debug)]
pub enum CardBytes<'a> {
    Borrowed(&'a [u8]),
    BorrowedMut(&'a mut [u8]),
    Owned(Vec<u8>),
}

impl CardBytes<'_> {
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::BorrowedMut(bytes) => bytes,
            Self::Owned(bytes) => bytes,
        }
    }

    pub fn as_mut_slice(&mut self) -> Option<&mut [u8]> {
        match self {
            Self::Borrowed(_) => None,
            Self::BorrowedMut(bytes) => Some(bytes),
            Self::Owned(bytes) => Some(bytes),
        }
    }

    pub fn into_owned(self) -> Vec<u8> {
        match self {
            Self::Borrowed(bytes) => bytes.to_vec(),
            Self::BorrowedMut(bytes) => bytes.to_vec(),
            Self::Owned(bytes) => bytes,
        }
    }
}

pub trait RawMemoryCardImage<'a>: Sized {
    type Error;

    type Save<'card>;

    const BLOCK_SIZE: u64;

    const MAX_BLOCKS: u32;

    /// Create an empty, owned memory-card image.
    ///
    /// `blocks` must be at most [`Self::MAX_BLOCKS`].
    fn empty(blocks: u32) -> Result<Self, Self::Error>;

    /// Create an immutable memory-card view over `bytes`.
    ///
    /// [`Self::BLOCK_SIZE`] * `blocks` must fit into `bytes`.
    fn new(bytes: &'a [u8], blocks: u32) -> Result<Self, Self::Error>;

    /// Create a mutable memory-card view over `bytes`.
    ///
    /// [`Self::BLOCK_SIZE`] * `blocks` must fit into `bytes`.
    fn new_mut(bytes: &'a mut [u8], blocks: u32) -> Result<Self, Self::Error>;

    /// Materialize the complete raw image, copying borrowed backing when necessary.
    fn into_owned(self) -> Vec<u8>;

    /// Enumerate the saves in the raw memory card.
    fn saves(&self) -> Result<Vec<Self::Save<'_>>, Self::Error>;

    /// Append a save to the memory card.
    fn append_save(&mut self, save: Self::Save<'_>) -> Result<(), Self::Error>;
}

#[derive(Clone)]
pub(crate) struct ReconcileWorker {
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
    pub(crate) fn spawn(
        thread_name: &str,
        error_path: &str,
        reconcile: impl Fn() -> Result<(), Error> + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let (commands, pending) = mpsc::channel();
        let dirty_enqueued = Arc::new(AtomicBool::new(false));
        let background_enqueued = dirty_enqueued.clone();
        let thread = std::thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || reconcile_loop(pending, background_enqueued, reconcile))
            .map_err(|source| Error::Io {
                path: error_path.to_owned(),
                source,
            })?;
        Ok(Self {
            commands,
            dirty_enqueued,
            thread: Arc::new(Mutex::new(Some(thread))),
        })
    }

    pub(crate) fn schedule(&self) {
        if !self.dirty_enqueued.swap(true, Ordering::AcqRel) {
            let _ = self.commands.send(ReconcileCommand::Dirty);
        }
    }

    pub(crate) fn reconcile(&self) -> Result<(), Error> {
        let (finished, result) = mpsc::sync_channel(1);
        self.commands
            .send(ReconcileCommand::Sync(finished))
            .map_err(|_| Error::Unsupported("writeback worker stopped".to_owned()))?;
        result
            .recv()
            .map_err(|_| Error::Unsupported("writeback worker stopped".to_owned()))?
    }

    pub(crate) fn shutdown(&self) -> Result<(), Error> {
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
}

fn reconcile_loop(
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

#[cfg(test)]
mod writeback_tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn batches_dirty_writes_and_reports_sync_failures() {
        let calls = Arc::new(AtomicUsize::new(0));
        let background_calls = calls.clone();
        let worker = ReconcileWorker::spawn("writeback-batch-test", "test", move || {
            background_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .unwrap();
        worker.schedule();
        worker.schedule();
        worker.schedule();
        std::thread::sleep(Duration::from_millis(500));
        worker.reconcile().unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        worker.shutdown().unwrap();

        let failing = ReconcileWorker::spawn("writeback-error-test", "test", || {
            Err(Error::Unsupported("writeback failed".to_owned()))
        })
        .unwrap();
        failing.schedule();
        assert!(matches!(
            failing.shutdown(),
            Err(Error::Unsupported(message)) if message == "writeback failed"
        ));
    }
}
