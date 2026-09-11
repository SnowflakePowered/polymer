/// The local executor is unavailable or its outstanding-work budget is full.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplyClosed;

/// One receiver shared by asynchronous, blocking and GUI-polling callers.
pub type PendingReply<T, E> = tokio::sync::oneshot::Receiver<Result<T, E>>;

/// Admission ownership travels with each message. Native handlers release it
/// on entry; local bridges transfer it into the delegated completion.
pub struct Queued<M> {
    pub message: M,
    pub permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

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

impl Admission {
    pub async fn admit<M>(
        &self,
        message: M,
        attempts: usize,
        timeout: std::time::Duration,
    ) -> Result<Queued<M>, crate::AdmissionFailure> {
        for attempt in 1..=attempts.max(1) {
            match tokio::time::timeout(timeout, self.0.clone().acquire_owned()).await {
                Ok(Ok(permit)) => {
                    return Ok(Queued {
                        message,
                        permit: Some(permit),
                    });
                }
                Ok(Err(_)) => return Err(crate::AdmissionFailure::Closed),
                Err(_) if attempt < attempts.max(1) => tokio::task::yield_now().await,
                Err(_) => return Err(crate::AdmissionFailure::Timeout),
            }
        }
        unreachable!("at least one admission attempt")
    }
}
