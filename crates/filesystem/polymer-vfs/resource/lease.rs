use crate::VfsResult;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FinalizationReason {
    LaunchFailed,
    ProcessExited,
    Cancelled,
}

pub trait ResourceLease: Send + Sync {
    fn finalize(&self, reason: FinalizationReason) -> VfsResult<()>;
}
