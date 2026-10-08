use std::sync::Arc;

use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::Error;
use crate::storage::operation::StorageOperation;

pub(super) struct ComposedOperation {
    pub(super) gate: RwLock<()>,
    domain: Option<Arc<StorageOperation>>,
}

impl ComposedOperation {
    pub(super) fn new(domain: Option<Arc<StorageOperation>>) -> Self {
        Self {
            gate: RwLock::new(()),
            domain,
        }
    }

    pub(super) fn read(&self) -> Result<RwLockReadGuard<'_, ()>, Error> {
        let guard = self.gate.read();
        if let Some(domain) = &self.domain {
            domain.ensure_recovered()?;
        }
        Ok(guard)
    }

    pub(super) fn write(&self) -> Result<RwLockWriteGuard<'_, ()>, Error> {
        let guard = self.gate.write();
        if let Some(domain) = &self.domain {
            domain.ensure_ready()?;
        }
        Ok(guard)
    }
}
