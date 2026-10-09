mod exclusive;
mod lease;
mod managed;
mod namespace_file;
mod prepared;
mod retry;

use crate::{Permissions, VfsResult};
use std::{fmt, sync::Arc};

pub use exclusive::ExclusiveResourceKey;
pub use lease::{FinalizationReason, ResourceLease};
pub use managed::{ManagedResourceLease, MaterializationScope, ResourceFinalizer};
pub use prepared::PreparedFilesystem;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceOrigin {
    Memory,
    Scratch,
    Cache(String),
    Package(String),
    Asar(String),
    HostFile,
    Other(String),
}

pub struct Resource<T: ?Sized> {
    factory: Arc<dyn ResourceFactory<T>>,
    authority: Permissions,
    origin: ResourceOrigin,
}

impl<T: ?Sized> Resource<T> {
    pub fn new(
        factory: Arc<dyn ResourceFactory<T>>,
        authority: Permissions,
        origin: ResourceOrigin,
    ) -> Self {
        Self {
            factory,
            authority,
            origin,
        }
    }

    pub const fn authority(&self) -> Permissions {
        self.authority
    }

    pub fn origin(&self) -> &ResourceOrigin {
        &self.origin
    }

    pub fn materialize(&self) -> VfsResult<Materialized<T>> {
        self.factory.materialize()
    }

    pub fn materialize_in(&self, scope: &mut MaterializationScope) -> VfsResult<Arc<T>> {
        scope.build(&*self.factory)
    }

    pub(crate) fn key(self: &Arc<Self>) -> usize {
        Arc::as_ptr(self) as *const () as usize
    }
}

impl<T: ?Sized> fmt::Debug for Resource<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Resource")
            .field("authority", &self.authority)
            .field("origin", &self.origin)
            .finish_non_exhaustive()
    }
}

pub trait ResourceFactory<T: ?Sized>: Send + Sync {
    fn resource_keys(&self) -> Vec<ExclusiveResourceKey> {
        Vec::new()
    }

    fn materialize_in(&self, scope: &mut MaterializationScope) -> VfsResult<Arc<T>>;

    fn materialize(&self) -> VfsResult<Materialized<T>> {
        let mut scope = MaterializationScope::new()?;
        let backend = scope.build(self)?;
        Ok(Materialized {
            backend,
            lease: scope.into_lease(),
        })
    }
}

pub struct Materialized<T: ?Sized> {
    pub backend: Arc<T>,
    pub lease: ManagedResourceLease,
}
