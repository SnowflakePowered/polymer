use super::super::writer::StorageWriter;
use super::{Ps3PupDepot, sce::Sce};
use crate::installable::DepotFile;
use crate::installable::Installable;
use crate::installable::artifacts::{Artifact, DescribeArtifact};
use crate::{Error, VolumeStorage};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;

/// Writes pending NAND changes and records one firmware artifact at completion.
/// Failed or cancelled installations are discarded by the NAND session finish().
pub(super) struct PupInstall {
    file: File,
    packages: Vec<(String, u64, u64)>,
    artifact: Artifact,
    manifest: Vec<DepotFile>,
    paths: std::collections::BTreeSet<String>,
    index: usize,
    storage: Arc<dyn VolumeStorage>,
}
impl PupInstall {
    pub async fn new(depot: &Ps3PupDepot, storage: Arc<dyn VolumeStorage>) -> Result<Self, Error> {
        let depot = depot.clone();
        tokio::task::spawn_blocking(move || {
            let mut pup = depot.verified_pup()?;
            let (offset, size) = pup.entry(0x300).map_err(|e| Error::io(&depot.path, e))?;
            let mut archive = tar::Archive::new((&mut pup.file).take(size));
            let mut packages = Vec::new();
            for entry in archive.entries().map_err(|e| Error::io(&depot.path, e))? {
                let entry = entry.map_err(|e| Error::io(&depot.path, e))?;
                let name = entry
                    .path()
                    .map_err(|e| Error::io(&depot.path, e))?
                    .to_string_lossy()
                    .into_owned();
                if !name.starts_with("dev_flash_") {
                    continue;
                }
                if !entry.header().entry_type().is_file() || entry.size() > Sce::MAX_SIZE {
                    return Err(Error::malformed("PUP", "invalid flash package"));
                }
                if entry
                    .raw_file_position()
                    .checked_add(entry.size())
                    .is_none_or(|end| end > size)
                {
                    return Err(Error::malformed("PUP", "flash package exceeds update TAR"));
                }
                packages.push((name, offset + entry.raw_file_position(), entry.size()));
            }
            if packages.is_empty() {
                return Err(Error::malformed("PUP", "no flash packages"));
            }
            Ok(Self {
                file: pup.file,
                packages,
                artifact: depot.describe(),
                manifest: Vec::new(),
                paths: std::collections::BTreeSet::new(),
                index: 0,
                storage,
            })
        })
        .await
        .map_err(|e| Error::Unsupported(format!("PUP worker failed: {e}")))?
    }
    fn extract(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let mut archive = tar::Archive::new(bytes);
        for entry in archive
            .entries()
            .map_err(|e| Error::malformed("flash TAR", e.to_string()))?
        {
            let mut entry = entry.map_err(|e| Error::malformed("flash TAR", e.to_string()))?;
            let path = entry
                .path()
                .map_err(|e| Error::malformed("flash TAR", e.to_string()))?
                .to_str()
                .ok_or_else(|| Error::malformed("flash TAR", "non-UTF-8 path"))?
                .trim_end_matches('/')
                .to_owned();
            super::super::pkg::Ps3PkgDepot::validate_path(&path)?;
            if path != "dev_flash" && !path.starts_with("dev_flash/") {
                return Err(Error::InvalidPath(path));
            }
            let kind = entry.header().entry_type();
            if kind.is_dir() {
                self.storage.create_dir_all(&path)?;
                continue;
            }
            if !kind.is_file() {
                return Err(Error::malformed("flash TAR", "unsupported entry type"));
            }
            if !self.paths.insert(path.to_lowercase()) {
                return Err(Error::malformed("flash TAR", "duplicate firmware file"));
            }
            let mut writer = StorageWriter {
                file: self.storage.create(&path)?,
                position: 0,
                hash: blake3::Hasher::new(),
            };
            io::copy(&mut entry, &mut writer)
                .and_then(|_| writer.flush())
                .map_err(|e| Error::io(&PathBuf::from(&path), e))?;
            if writer.position != entry.size() {
                return Err(Error::malformed("flash TAR", "truncated firmware file"));
            }
            self.manifest.push(DepotFile::new(
                PathBuf::from(path),
                *writer.hash.finalize().as_bytes(),
                writer.position,
            ));
        }
        Ok(())
    }
}

impl PupInstall {
    fn next_file(&mut self) -> Result<String, Error> {
        let (name, offset, size) = &self.packages[self.index];
        let path = PathBuf::from(name);
        self.file
            .seek(SeekFrom::Start(*offset))
            .map_err(|e| Error::io(&path, e))?;
        let mut bytes = vec![0; *size as usize];
        self.file
            .read_exact(&mut bytes)
            .map_err(|e| Error::io(&path, e))?;
        let bytes = Sce::flash_tar(&bytes).map_err(|e| Error::io(&path, e))?;
        self.extract(&bytes)?;
        let name = self.packages[self.index].0.clone();
        self.index += 1;
        Ok(name)
    }
}

impl Installable for PupInstall {
    fn artifact(&self) -> &Artifact {
        &self.artifact
    }
    async fn next_batch(mut self) -> Result<(Self, Option<Vec<DepotFile>>), Error> {
        if self.index == self.packages.len() {
            return Ok((self, None));
        }
        tokio::task::spawn_blocking(move || {
            self.next_file()?;
            let files = std::mem::take(&mut self.manifest);
            Ok((self, Some(files)))
        })
        .await
        .map_err(|error| Error::Unsupported(format!("PUP worker failed: {error}")))?
    }
    async fn finish(self, artifact: Artifact) -> Result<Artifact, Error> {
        if artifact.manifest().is_empty() {
            return Err(Error::malformed("PUP", "empty firmware image"));
        }
        // A firmware image replaces the previous image. Remove only files owned
        // by its previous artifact, preserving unrelated NAND contents.
        if let Some(previous) = self.storage.artifacts().artifact(artifact.id()).await? {
            for file in previous.manifest() {
                if !artifact
                    .manifest()
                    .iter()
                    .any(|new| new.path() == file.path())
                {
                    let path = file.path().to_string_lossy();
                    if path.starts_with("dev_flash/") && self.storage.exists(&path) {
                        self.storage.remove(&path)?;
                    }
                }
            }
        }
        self.storage.artifacts().record(&artifact).await?;
        Ok(artifact)
    }
}
