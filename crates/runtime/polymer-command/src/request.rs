/// The local executor is unavailable or its outstanding-work budget is full.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplyClosed;

/// One receiver shared by asynchronous, blocking and GUI-polling callers.
pub type PendingReply<T, E> = tokio::sync::oneshot::Receiver<Result<T, E>>;

/// Shared by endpoints feeding the same local executor.
#[derive(Clone, Debug)]
pub struct Admission(pub(crate) std::sync::Arc<tokio::sync::Semaphore>);

impl Admission {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self(std::sync::Arc::new(tokio::sync::Semaphore::new(capacity)))
    }

    pub fn close(&self) {
        self.0.close();
    }
}

impl Default for Admission {
    fn default() -> Self {
        Self::new(64)
    }
}
