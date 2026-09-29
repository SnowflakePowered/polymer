use crate::Error;
use crate::installable::{DepotFile, DepotType, IconHint};
use jiff::Timestamp;

/// The file an emulator launches, relative to the artifact's install root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactEntrypoint {
    pub root: String,
    pub path: String,
}

#[derive(Clone)]
pub struct Artifact {
    id: String,
    depot_type: DepotType,
    manifest: Vec<DepotFile>,
    label: String,
    version: Option<String>,
    icons: Vec<IconHint>,
    byte_length: u64,
    installed: Timestamp,
    entrypoint: Option<ArtifactEntrypoint>,
}

impl Artifact {
    pub fn new(
        id: String,
        depot_type: DepotType,
        manifest: Vec<DepotFile>,
        label: String,
        version: Option<String>,
        icons: Vec<IconHint>,
    ) -> Self {
        let byte_length = manifest.iter().map(DepotFile::byte_length).sum();
        Self {
            id,
            depot_type,
            manifest,
            label,
            version,
            icons,
            byte_length,
            installed: Timestamp::now(),
            entrypoint: None,
        }
    }

    /// Records the launch file, which must be one of this artifact's installed files.
    pub fn with_entrypoint(mut self, entrypoint: ArtifactEntrypoint) -> Result<Self, Error> {
        let installed = format!("{}/{}", entrypoint.root, entrypoint.path);
        if !self
            .manifest
            .iter()
            .any(|file| file.path().to_string_lossy().replace('\\', "/") == installed)
        {
            return Err(Error::malformed(
                "artifact",
                format!("entrypoint `{installed}` is not an installed file"),
            ));
        }
        self.entrypoint = Some(entrypoint);
        Ok(self)
    }

    pub(crate) fn with_installed(mut self, installed: Timestamp) -> Self {
        self.installed = installed;
        self
    }

    pub(crate) fn with_recorded_entrypoint(
        mut self,
        entrypoint: Option<ArtifactEntrypoint>,
    ) -> Self {
        self.entrypoint = entrypoint;
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

    pub fn icons(&self) -> &[IconHint] {
        &self.icons
    }

    pub fn byte_length(&self) -> u64 {
        self.byte_length
    }

    pub fn installed(&self) -> Timestamp {
        self.installed
    }

    pub fn entrypoint(&self) -> Option<&ArtifactEntrypoint> {
        self.entrypoint.as_ref()
    }
}
