use crate::Error;
use crate::installable::{DepotFile, DepotType, IconHint};
use jiff::Timestamp;

/// The file an emulator launches, relative to the artifact's install root, and
/// the NAND path of the installed package that identifies the title.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArtifactEntrypoint {
    pub root: String,
    #[serde(rename = "entrypoint")]
    pub path: String,
    pub package: String,
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
        entrypoint: Option<ArtifactEntrypoint>,
    ) -> Result<Self, Error> {
        if let Some(entrypoint) = &entrypoint {
            let installed = format!("{}/{}", entrypoint.root, entrypoint.path);
            let package = format!("{}/", entrypoint.package);
            let paths: Vec<_> = manifest
                .iter()
                .map(|file| file.path().to_string_lossy().replace('\\', "/"))
                .collect();
            if !paths.contains(&installed) {
                return Err(Error::malformed(
                    "artifact",
                    format!("entrypoint `{installed}` is not an installed file"),
                ));
            }
            if !paths
                .iter()
                .any(|path| *path == entrypoint.package || path.starts_with(&package))
            {
                return Err(Error::malformed(
                    "artifact",
                    format!("package `{}` is not installed", entrypoint.package),
                ));
            }
        }
        let byte_length = manifest.iter().map(DepotFile::byte_length).sum();
        Ok(Self {
            id,
            depot_type,
            manifest,
            label,
            version,
            icons,
            byte_length,
            installed: Timestamp::now(),
            entrypoint,
        })
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

    pub fn entrypoint(&self) -> Option<&ArtifactEntrypoint> {
        self.entrypoint.as_ref()
    }
}
