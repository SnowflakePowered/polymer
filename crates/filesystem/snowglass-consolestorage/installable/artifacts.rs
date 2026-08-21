use crate::installable::{DepotFile, DepotType, IconHint};

pub struct Artifact {
    /// Identifies the artifact to the NAND it is installed in. Wii U NANDs use
    /// the 16 hex digit title id.
    id: String,
    depot_type: DepotType,
    manifest: Vec<DepotFile>,
    label: String,
    version: Option<String>,
    icon: IconHint,
    /// The total byte length of the artifact installed on disk.
    byte_length: u64,
}

/// Supplies everything about an [`Artifact`] that the install walk cannot
/// work out for itself. The returned artifact carries no manifest; the walk
/// fills that in with [`Artifact::with_manifest`].
pub trait DescribeArtifact {
    fn describe(&self) -> Artifact;
}

impl Artifact {
    pub fn new(
        id: String,
        depot_type: DepotType,
        manifest: Vec<DepotFile>,
        label: String,
        version: Option<String>,
        icon: IconHint,
    ) -> Self {
        let byte_length = manifest.iter().map(DepotFile::byte_length).sum();
        Self {
            id,
            depot_type,
            manifest,
            label,
            version,
            icon,
            byte_length,
        }
    }

    /// Replace the manifest, recomputing the installed size.
    pub fn with_manifest(mut self, manifest: Vec<DepotFile>) -> Self {
        self.byte_length = manifest.iter().map(DepotFile::byte_length).sum();
        self.manifest = manifest;
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn depot_type(&self) -> DepotType {
        self.depot_type
    }

    pub fn manifest(&self) -> &[DepotFile] {
        &self.manifest
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    pub fn icon(&self) -> &IconHint {
        &self.icon
    }

    pub fn byte_length(&self) -> u64 {
        self.byte_length
    }
}
