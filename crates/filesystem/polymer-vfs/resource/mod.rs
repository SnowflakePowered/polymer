mod exclusive;
mod lease;
mod namespace_file;
mod prepared;

use crate::{Permissions, VfsResult};
use std::{fmt, sync::Arc};

pub use exclusive::ExclusiveResourceKey;
pub use lease::{CompositeResourceLease, FinalizationReason, NoopResourceLease, ResourceLease};
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
    fn materialize(&self) -> VfsResult<Materialized<T>>;
}

pub struct Materialized<T: ?Sized> {
    pub backend: Arc<T>,
    pub lease: Arc<dyn ResourceLease>,
}
