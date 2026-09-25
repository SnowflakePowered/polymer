use crate::installable::{DepotFile, DepotType, IconHint};
use jiff::Timestamp;

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
        }
    }

    pub(crate) fn with_installed(mut self, installed: Timestamp) -> Self {
        self.installed = installed;
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
}
