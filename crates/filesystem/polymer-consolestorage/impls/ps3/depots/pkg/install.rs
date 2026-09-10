use crate::installable::Installation;
use futures::channel::oneshot;
use futures::{TryStreamExt, stream};
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
    artifact: Option<Artifact>,
    manifest: Vec<DepotFile>,
    index: usize,
    storage: Arc<dyn VolumeStorage>,
}

impl Ps3PkgInstall {
    pub fn run(
        mut self,
        outcome: oneshot::Sender<Result<Artifact, Error>>,
    ) -> Installation<'static> {
        let artifact = self
            .artifact
            .take()
            .unwrap()
            .with_manifest(std::mem::take(&mut self.manifest));
        let storage = self.storage.clone();
        let batches = stream::try_unfold(self, |mut install| async move {
            if install.index == install.archive.items().len() {
                return Ok::<_, Error>(None);
            }
            tokio::task::spawn_blocking(move || {
                install.next_file()?;
                let files = std::mem::take(&mut install.manifest);
                Ok(Some((stream::iter(files.into_iter().map(Ok)), install)))
            })
            .await
            .map_err(|error| Error::Unsupported(format!("PKG worker failed: {error}")))?
        });
        Installation::new(
            batches.try_flatten(),
            artifact,
            move |artifact| async move {
                storage.artifacts().record(&artifact).await?;
                Ok(artifact)
            },
            outcome,
        )
    }

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
                artifact: Some(depot.describe()),
                manifest: Vec::new(),
                index: 0,
                storage,
            })
        })
        .await
        .map_err(|error| Error::Unsupported(format!("PKG worker failed: {error}")))??;
        // Reinstalling a patch is additive. Retain ownership of earlier files.
        let id = install.artifact.as_ref().unwrap().id();
        if let Some(old) = install.storage.artifacts().artifact(id).await? {
            install.manifest = old
                .manifest()
                .iter()
                .map(|file| {
                    DepotFile::new(
                        file.path().to_path_buf(),
                        *file.blake3(),
                        file.byte_length(),
                    )
                })
                .collect();
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
