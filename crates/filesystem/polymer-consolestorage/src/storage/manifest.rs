use jiff::Timestamp;
use uuid::Uuid;

use super::BoxFuture;
use crate::error::Error;
use crate::installable::DepotType;
use crate::installable::artifacts::{Artifact, ArtifactEntrypoint};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactSummary {
    pub id: String,
    pub depot_type: DepotType,
    pub label: String,
    pub version: Option<String>,
    pub byte_length: u64,
    pub installed: Timestamp,
    pub file_count: usize,
    pub icons: Vec<String>,
    #[serde(flatten)]
    pub entrypoint: Option<ArtifactEntrypoint>,
}

impl From<&Artifact> for ArtifactSummary {
    fn from(artifact: &Artifact) -> Self {
        Self {
            id: artifact.id().to_owned(),
            depot_type: artifact.depot_type(),
            label: artifact.label().to_owned(),
            version: artifact.version().map(str::to_owned),
            byte_length: artifact.byte_length(),
            installed: artifact.installed(),
            file_count: artifact.manifest().len(),
            icons: artifact
                .icons()
                .iter()
                .filter_map(|icon| match icon {
                    crate::installable::IconHint::Embedded(icon) => Some(icon.data_url()),
                    _ => None,
                })
                .collect(),
            entrypoint: artifact.entrypoint().cloned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    pub id: Uuid,
    pub platform: String,
    pub label: Option<String>,
    pub created: Timestamp,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ArtifactComponent {
    pub id: String,
    pub title: String,
    pub root: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ArtifactRevision {
    pub revision: String,
    pub artifact: ArtifactSummary,
}

pub trait ArtifactRegistry: Send + Sync {
    fn install_component<'a>(
        &'a self,
        _component: &'a ArtifactComponent,
        _metadata: &'a serde_json::Value,
        _artifact: &'a Artifact,
        _source: std::sync::Arc<dyn super::Storage>,
        _context: &'a crate::installable::InstallContext,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async { Err(Error::Unsupported("component installation".into())) })
    }

    fn history<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<Vec<ArtifactRevision>, Error>> {
        Box::pin(async { Err(Error::Unsupported("component history".into())) })
    }

    fn restore<'a>(&'a self, _id: &'a str, _revision: &'a str) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async { Err(Error::Unsupported("component restore".into())) })
    }

    fn revision(&self) -> Result<String, Error>;
    fn affected<'a>(&'a self, ids: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<String>, Error>>;
    /// Commit the artifact and pending files, reporting a finalizing phase while blobs are written.
    fn record<'a>(
        &'a self,
        artifact: &'a Artifact,
        context: &'a crate::installable::InstallContext,
    ) -> BoxFuture<'a, Result<(), Error>>;
    fn summaries(&self) -> BoxFuture<'_, Result<Vec<ArtifactSummary>, Error>>;
    fn artifact<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Artifact>, Error>>;
    fn contains<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, Error>>;
    fn uninstall<'a>(&'a self, ids: &'a [&'a str]) -> BoxFuture<'a, Result<(), Error>>;
}
