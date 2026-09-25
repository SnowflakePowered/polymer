use jiff::Timestamp;
use uuid::Uuid;

use super::BoxFuture;
use crate::error::Error;
use crate::installable::DepotType;
use crate::installable::artifacts::Artifact;

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
    pub icon: Option<String>,
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
            icon: match artifact.icon() {
                crate::installable::IconHint::EmbeddedPng(png) => Some(ArtifactSummary::icon_url(png)),
                _ => None,
            },
        }
    }
}

impl ArtifactSummary {
    pub(crate) fn icon_url(png: &[u8]) -> String {
        use base64::Engine as _;
        format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    pub id: Uuid,
    pub platform: String,
    pub label: Option<String>,
    pub created: Timestamp,
}

pub trait ArtifactRegistry: Send + Sync {
    fn revision(&self) -> Result<String, Error>;
    fn affected<'a>(&'a self, ids: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<String>, Error>>;
    /// Commit the artifact and pending files, publishing a finalizing phase while blobs are written.
    fn record<'a>(
        &'a self,
        artifact: &'a Artifact,
        events: crate::installable::InstallEvents,
    ) -> BoxFuture<'a, Result<(), Error>>;
    fn summaries(&self) -> BoxFuture<'_, Result<Vec<ArtifactSummary>, Error>>;
    fn artifact<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Artifact>, Error>>;
    fn contains<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, Error>>;
    fn uninstall<'a>(&'a self, ids: &'a [&'a str]) -> BoxFuture<'a, Result<(), Error>>;
}
