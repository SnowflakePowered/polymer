use jiff::Timestamp;
use uuid::Uuid;

use super::BoxFuture;
use crate::error::Error;
use crate::installable::DepotType;
use crate::installable::artifacts::Artifact;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSummary {
    pub id: String,
    pub depot_type: DepotType,
    pub label: String,
    pub version: Option<String>,
    pub byte_length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    pub id: Uuid,
    pub platform: String,
    pub label: Option<String>,
    pub created: Timestamp,
}

pub trait ArtifactRegistry: Send + Sync {
    fn record<'a>(&'a self, artifact: &'a Artifact) -> BoxFuture<'a, Result<(), Error>>;
    fn summaries(&self) -> BoxFuture<'_, Result<Vec<ArtifactSummary>, Error>>;
    fn artifact<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Artifact>, Error>>;
    fn contains<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, Error>>;
    fn uninstall<'a>(&'a self, ids: &'a [&'a str]) -> BoxFuture<'a, Result<(), Error>>;
}

#[derive(Debug)]
pub(crate) struct EmptyArtifactRegistry;

pub(crate) static EMPTY_ARTIFACTS: EmptyArtifactRegistry = EmptyArtifactRegistry;

impl ArtifactRegistry for EmptyArtifactRegistry {
    fn record<'a>(&'a self, _artifact: &'a Artifact) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async { Ok(()) })
    }

    fn summaries(&self) -> BoxFuture<'_, Result<Vec<ArtifactSummary>, Error>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn artifact<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<Option<Artifact>, Error>> {
        Box::pin(async { Ok(None) })
    }

    fn contains<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<bool, Error>> {
        Box::pin(async { Ok(false) })
    }

    fn uninstall<'a>(&'a self, _ids: &'a [&'a str]) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async { Ok(()) })
    }
}
