use std::time::{Duration, Instant};

/// Native state remains on the caller's graphics context/thread. Implementations
/// own synchronized destruction through Drop; no lifecycle method dispatches work
/// to another thread.
pub trait CaptureBackend {
    /// Whether the negotiated consumer can still receive frames. This must not
    /// allocate resources or submit GPU work.
    fn connected(&self) -> bool;
}

/// Backend-specific negotiation, allocation and publication performed inline.
/// Return None for rejection and Err for a transient setup failure.
pub trait CaptureInitializer {
    type Backend: CaptureBackend;
    type Error;
    /// # Safety
    /// The caller must satisfy the backend's native resource lifetime and
    /// thread/context requirements for the duration of initialization.
    unsafe fn initialize(&self) -> Result<Option<Self::Backend>, Self::Error>;
}

/// A native capture operation, borrowing only caller-owned context and inputs.
/// None denotes a skipped/busy frame; an output denotes submitted work.
pub trait CaptureOperation {
    type Backend: CaptureBackend;
    type Output;
    type Error: std::fmt::Debug;

    /// # Safety
    /// The caller must keep native inputs alive and execute on the graphics
    /// context/thread required by this operation.
    unsafe fn submit(
        self,
        backend: &mut Self::Backend,
    ) -> Result<Option<Self::Output>, Self::Error>;
}

/// Per-stream state and capture requirements. Native ownership is private so
/// providers cannot bypass invalidation, retry, or rejection transitions.
pub struct CaptureSession<B, C = ()> {
    state: LifecycleState<B>,
    configuration: Option<C>,
    counters: CaptureCounters,
}

/// Operation accounting is separate from published frames and GPU sync values.
/// A split capture stage (such as acquiring a paint DC) is an operation too.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptureCounters {
    pub completed_operations: u64,
    pub skipped_operations: u64,
    pub failures: u64,
}
impl CaptureCounters {
    fn record<B, O, E: std::fmt::Debug>(&mut self, result: &Result<Option<O>, E>) {
        match result {
            Ok(Some(_)) => self.completed_operations = self.completed_operations.saturating_add(1),
            Ok(None) => self.skipped_operations = self.skipped_operations.saturating_add(1),
            Err(error) => {
                self.failures = self.failures.saturating_add(1);
                if self.failures == 1 || self.failures.is_multiple_of(120) {
                    tracing::debug!(
                        backend = std::any::type_name::<B>(),
                        failures = self.failures,
                        ?error,
                        "polymer: capture operation failed"
                    );
                }
            }
        }
    }
}
#[derive(Default)]
enum LifecycleState<B> {
    #[default]
    Idle,
    Initializing(Instant),
    Active(Box<B>),
    Parked,
    Retiring(Box<B>),
}
impl<B, C> Default for CaptureSession<B, C> {
    fn default() -> Self {
        Self {
            state: LifecycleState::Idle,
            configuration: None,
            counters: CaptureCounters::default(),
        }
    }
}
impl<B: CaptureBackend, C> CaptureSession<B, C> {
    pub fn configuration(&self) -> Option<&C> {
        self.configuration.as_ref()
    }

    pub fn counters(&self) -> CaptureCounters {
        self.counters
    }
    pub fn from_backend(backend: B) -> Self {
        let mut session = Self::default();
        let _ = session.initialized::<()>(Ok(Some(backend)));
        session
    }
    pub fn is_parked(&self) -> bool {
        matches!(self.state, LifecycleState::Parked)
    }

    /// A new source can retry a parked stream without rebuilding compatible
    /// active resources on ordinary resource page flips.
    pub fn source_changed(&mut self) {
        if self.is_parked() {
            self.invalidate();
        }
    }
    /// Dispatches inline only while an accepted consumer remains connected.
    /// # Safety
    /// The operation's native input lifetime and context requirements must hold.
    pub unsafe fn capture<O>(&mut self, operation: O) -> Result<Option<O::Output>, O::Error>
    where
        O: CaptureOperation<Backend = B>,
    {
        self.refresh_connection();
        let Some(backend) = self.as_mut() else {
            self.counters.skipped_operations = self.counters.skipped_operations.saturating_add(1);
            return Ok(None);
        };
        let result = unsafe { operation.submit(backend) };
        self.counters.record::<B, _, _>(&result);
        self.refresh_connection();
        result
    }
    pub fn as_ref(&self) -> Option<&B> {
        if let LifecycleState::Active(value) = &self.state {
            Some(value)
        } else {
            None
        }
    }
    pub fn as_mut(&mut self) -> Option<&mut B> {
        if let LifecycleState::Active(value) = &mut self.state {
            Some(value)
        } else {
            None
        }
    }
    fn begin(&mut self, now: Instant) -> bool {
        self.refresh_connection();
        let eligible = match self.state {
            LifecycleState::Idle => true,
            LifecycleState::Initializing(start) => {
                now.saturating_duration_since(start) >= Duration::from_secs(1)
            }
            _ => false,
        };
        if eligible {
            self.state = LifecycleState::Initializing(now);
        }
        eligible
    }
    /// Initializes on the caller's thread with one-second retry backoff.
    /// # Safety
    /// The initializer's native lifetime and context requirements must hold.
    pub unsafe fn initialize<I>(&mut self, now: Instant, initializer: &I) -> Result<(), I::Error>
    where
        I: CaptureInitializer<Backend = B>,
    {
        if !self.begin(now) {
            return Ok(());
        }
        self.initialized(unsafe { initializer.initialize() })
    }
    fn initialized<E>(&mut self, result: Result<Option<B>, E>) -> Result<(), E> {
        match result? {
            Some(backend) if backend.connected() => {
                self.state = LifecycleState::Active(Box::new(backend))
            }
            _ => self.park(),
        }
        Ok(())
    }
    pub fn refresh_connection(&mut self) {
        if self.as_ref().is_some_and(|backend| !backend.connected()) {
            self.park();
        }
    }
    pub fn park(&mut self) {
        self.invalidate();
        self.state = LifecycleState::Parked;
    }
    pub fn invalidate(&mut self) {
        let previous = std::mem::replace(&mut self.state, LifecycleState::Idle);
        if let LifecycleState::Active(active) = previous {
            self.state = LifecycleState::Retiring(active);
        }
        if let LifecycleState::Retiring(backend) =
            std::mem::replace(&mut self.state, LifecycleState::Idle)
        {
            drop(backend);
        }
    }
}
impl<B: CaptureBackend, C: PartialEq> CaptureSession<B, C> {
    /// Requirements persist while parked or retrying, so only an actual change
    /// rebuilds resources or clears rejection. The backend defines these data.
    pub fn configure(&mut self, proposed: C) -> bool {
        if self.configuration.as_ref() == Some(&proposed) {
            return false;
        }
        self.invalidate();
        self.configuration = Some(proposed);
        true
    }
}
#[cfg(test)]
mod tests;
