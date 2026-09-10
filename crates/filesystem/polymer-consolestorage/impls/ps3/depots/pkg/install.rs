use crate::installable::Installable;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use pkgrip::PkgArchive;

use super::super::writer::StorageWriter;
use super::Ps3PkgDepot;
use crate::installable::DepotFile;
use crate::installable::artifacts::{Artifact, DescribeArtifact};
use crate::{Error, VolumeStorage};

/// Streams an installation into the NAND working tree, then records one artifact
/// commit. Failed or cancelled writes are discarded by the NAND session finish().
pub(super) struct Ps3PkgInstall {
    archive: PkgArchive<File>,
    root: String,
    artifact: Artifact,
    manifest: Vec<DepotFile>,
    index: usize,
    storage: Arc<dyn VolumeStorage>,
}

impl Ps3PkgInstall {
    pub async fn new(depot: &Ps3PkgDepot, storage: Arc<dyn VolumeStorage>) -> Result<Self, Error> {
        let depot = depot.clone();
        let mut install = tokio::task::spawn_blocking(move || {
            let file = depot.verified_file()?;
            let size = file
                .metadata()
                .map_err(|error| Error::io(&depot.path, error))?
                .len();
            let archive = PkgArchive::from_reader(file, size)
                .map_err(|error| Error::io(&depot.path, error))?;
            Ok::<_, Error>(Self {
                archive,
                root: depot.install_path.clone(),
                artifact: depot.describe(),
                manifest: Vec::new(),
                index: 0,
                storage,
            })
        })
        .await
        .map_err(|error| Error::Unsupported(format!("PKG worker failed: {error}")))??;
        // Reinstalling a patch is additive. Retain ownership of earlier files.
        let id = install.artifact.id();
        if let Some(old) = install.storage.artifacts().artifact(id).await? {
            install.artifact = install.artifact.with_manifest(old.manifest().to_vec());
        }
        Ok(install)
    }

    fn record_file(&mut self, path: String, mut writer: StorageWriter) -> Result<(), Error> {
        writer
            .flush()
            .map_err(|error| Error::io(&PathBuf::from(&path), error))?;
        self.manifest
            .retain(|file| file.path() != std::path::Path::new(&path));
        self.manifest.push(DepotFile::new(
            PathBuf::from(path),
            *writer.hash.finalize().as_bytes(),
            writer.position,
        ));
        Ok(())
    }
}

impl Ps3PkgInstall {
    fn next_file(&mut self) -> Result<String, Error> {
        let item = &self.archive.items()[self.index];
        let path = format!("{}/{}", self.root, item.name);
        if item.is_file {
            let mut writer = StorageWriter {
                file: self.storage.create(&path)?,
                position: 0,
                hash: blake3::Hasher::new(),
            };
            self.archive
                .stream_item(self.index, &mut writer)
                .map_err(|error| Error::io(&PathBuf::from(&path), error))?;
            self.record_file(path.clone(), writer)?;
        } else {
            self.storage.create_dir_all(&path)?;
        }
        self.index += 1;
        Ok(path)
    }
}

impl Installable for Ps3PkgInstall {
    fn artifact(&self) -> &Artifact {
        &self.artifact
    }
    async fn next_batch(mut self) -> Result<(Self, Option<Vec<DepotFile>>), Error> {
        if self.index == self.archive.items().len() {
            return Ok((self, None));
        }
        tokio::task::spawn_blocking(move || {
            self.next_file()?;
            let files = std::mem::take(&mut self.manifest);
            Ok((self, Some(files)))
        })
        .await
        .map_err(|error| Error::Unsupported(format!("PKG worker failed: {error}")))?
    }
    async fn finish(self, artifact: Artifact) -> Result<Artifact, Error> {
        self.storage.artifacts().record(&artifact).await?;
        Ok(artifact)
    }
}
