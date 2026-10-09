use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;

use super::retry::FinalizationRetry;
use super::{ExclusiveResourceKey, FinalizationReason, ResourceFactory, ResourceLease};
use crate::VfsResult;

pub trait ResourceFinalizer: Send + Sync {
    fn finalize(&self, reason: FinalizationReason) -> VfsResult<()>;
}

pub struct MaterializationScope(ManagedResourceLease);

impl MaterializationScope {
    pub fn new() -> VfsResult<Self> {
        Ok(Self(ManagedResourceLease {
            pending: PendingFinalization(Mutex::new(Some(FinalizationState {
                ownership: Vec::new(),
                cleanup: VecDeque::new(),
                reason: None,
            }))),
            retry: Some(FinalizationRetry::reserve()?),
            fallback: FinalizationReason::LaunchFailed,
        }))
    }

    pub fn build<T, F>(&mut self, factory: &F) -> VfsResult<Arc<T>>
    where
        T: ?Sized,
        F: ResourceFactory<T> + ?Sized,
    {
        let ownership = factory
            .resource_keys()
            .into_iter()
            .map(ExclusiveResourceKey::acquire)
            .collect::<VfsResult<Vec<_>>>()?;
        self.state().ownership.extend(ownership);
        factory.materialize_in(self)
    }

    pub fn push(&mut self, finalizer: Arc<dyn ResourceFinalizer>) {
        self.state().cleanup.push_back(finalizer);
    }

    pub fn retain(&mut self, lease: Arc<dyn ResourceLease>) {
        self.state().ownership.push(lease);
    }

    pub fn into_lease(self) -> ManagedResourceLease {
        let mut lease = self.0;
        lease.fallback = FinalizationReason::Cancelled;
        lease
    }

    fn state(&mut self) -> &mut FinalizationState {
        self.0
            .pending
            .0
            .get_mut()
            .as_mut()
            .expect("active materialization scope")
    }
}

pub struct ManagedResourceLease {
    pending: PendingFinalization,
    retry: Option<FinalizationRetry>,
    fallback: FinalizationReason,
}

impl ResourceLease for ManagedResourceLease {
    fn finalize(&self, reason: FinalizationReason) -> VfsResult<()> {
        self.pending.finalize(reason)
    }
}

impl Drop for ManagedResourceLease {
    fn drop(&mut self) {
        if let Some(mut state) = self.pending.0.get_mut().take() {
            state.reason.get_or_insert(self.fallback);
            self.retry
                .take()
                .expect("reserved finalization worker")
                .retain(PendingFinalization(Mutex::new(Some(state))));
        }
    }
}

struct FinalizationState {
    ownership: Vec<Arc<dyn ResourceLease>>,
    cleanup: VecDeque<Arc<dyn ResourceFinalizer>>,
    reason: Option<FinalizationReason>,
}

impl FinalizationState {
    fn finalize(&mut self, reason: FinalizationReason) -> VfsResult<()> {
        let reason = *self.reason.get_or_insert(reason);
        while let Some(finalizer) = self.cleanup.front() {
            finalizer.finalize(reason)?;
            self.cleanup.pop_front();
        }
        self.ownership.clear();
        Ok(())
    }
}

struct PendingFinalization(Mutex<Option<FinalizationState>>);

impl ResourceLease for PendingFinalization {
    fn finalize(&self, reason: FinalizationReason) -> VfsResult<()> {
        let mut state = self.0.lock();
        if let Some(pending) = state.as_mut() {
            pending.finalize(reason)?;
            *state = None;
        }
        Ok(())
    }
}
