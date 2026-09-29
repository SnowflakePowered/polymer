use super::ExclusiveResourceKey;
use crate::VfsResult;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalizationReason {
    LaunchFailed,
    ProcessExited,
    Cancelled,
}

pub trait ResourceLease: Send + Sync {
    fn finalize(&self, reason: FinalizationReason) -> VfsResult<()>;
}

pub struct CompositeResourceLease {
    leases: Vec<Arc<dyn ResourceLease>>,
}

impl CompositeResourceLease {
    pub fn exclusive(ids: impl IntoIterator<Item = Uuid>) -> VfsResult<Self> {
        let leases = ids
            .into_iter()
            .map(|id| ExclusiveResourceKey::Resource(id).acquire())
            .collect::<VfsResult<Vec<_>>>()?;
        Ok(Self { leases })
    }

    pub fn with(mut self, lease: Arc<dyn ResourceLease>) -> Self {
        self.leases.push(lease);
        self
    }
}

impl ResourceLease for CompositeResourceLease {
    fn finalize(&self, reason: FinalizationReason) -> VfsResult<()> {
        let mut failure = None;
        for lease in self.leases.iter().rev() {
            if let Err(error) = lease.finalize(reason)
                && failure.is_none()
            {
                failure = Some(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

#[derive(Default)]
pub struct NoopResourceLease;

impl ResourceLease for NoopResourceLease {
    fn finalize(&self, _reason: FinalizationReason) -> VfsResult<()> {
        Ok(())
    }
}
