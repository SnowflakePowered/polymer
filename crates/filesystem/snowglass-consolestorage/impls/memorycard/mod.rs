use std::sync::{Arc, mpsc};

use parking_lot::Mutex;

use crate::Error;

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
    dirty: mpsc::SyncSender<()>,
    reconcile: Arc<dyn Fn() -> Result<(), Error> + Send + Sync>,
    running: Arc<Mutex<()>>,
}

impl ReconcileWorker {
    pub(crate) fn spawn(
        thread_name: &str,
        error_path: &str,
        reconcile: impl Fn() -> Result<(), Error> + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        let (dirty, pending) = mpsc::sync_channel(1);
        let reconcile: Arc<dyn Fn() -> Result<(), Error> + Send + Sync> = Arc::new(reconcile);
        let running = Arc::new(Mutex::new(()));
        let background_reconcile = reconcile.clone();
        let background_running = running.clone();
        std::thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                while pending.recv().is_ok() {
                    let _running = background_running.lock();
                    let _ = background_reconcile();
                }
            })
            .map_err(|source| Error::Io {
                path: error_path.to_owned(),
                source,
            })?;
        Ok(Self {
            dirty,
            reconcile,
            running,
        })
    }

    pub(crate) fn schedule(&self) {
        let _ = self.dirty.try_send(());
    }

    pub(crate) fn reconcile(&self) -> Result<(), Error> {
        let _running = self.running.lock();
        (self.reconcile)()
    }
}
