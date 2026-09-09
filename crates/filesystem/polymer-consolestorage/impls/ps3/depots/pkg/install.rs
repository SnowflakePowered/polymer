use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;

use futures::channel::oneshot;
use futures::{Stream, stream};
use pkgrip::PkgArchive;

use super::Ps3PkgDepot;
use crate::installable::DepotFile;
use crate::installable::artifacts::{Artifact, DescribeArtifact};
use crate::{Error, StorageFile, VolumeStorage};

/// Streams an installation into the NAND working tree, then records one artifact
/// commit. Failed or cancelled writes are discarded by the NAND session finish().
pub(super) struct Install {
    archive: PkgArchive<File>,
    root: String,
    artifact: Option<Artifact>,
    manifest: Vec<DepotFile>,
    index: usize,
    storage: Arc<dyn VolumeStorage>,
}

impl Install {
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

    pub fn run(
        self,
        sender: oneshot::Sender<Result<Artifact, Error>>,
    ) -> impl Stream<Item = Result<String, Error>> {
        stream::unfold(
            (Some(self), Some(sender)),
            |(install, mut sender)| async move {
                sender.as_ref()?;
                let mut install = install?;
                if install.index == install.archive.items().len() {
                    match install.finish().await {
                        Ok(artifact) => {
                            drop(install);
                            let _ = sender.take().unwrap().send(Ok(artifact));
                            return None;
                        }
                        Err(error) => {
                            let _ = sender.take().unwrap().send(Err(error.clone()));
                            return Some((Err(error), (Some(install), sender)));
                        }
                    }
                }
                // pkgrip is synchronous; keep decryption and disk I/O off the async executor.
                let step = tokio::task::spawn_blocking(move || {
                    let result = install.next_file();
                    (install, result)
                })
                .await;
                match step {
                    Ok((install, Ok(path))) => Some((Ok(path), (Some(install), sender))),
                    Ok((install, Err(error))) => {
                        let _ = sender.take().unwrap().send(Err(error.clone()));
                        Some((Err(error), (Some(install), sender)))
                    }
                    Err(error) => {
                        let error = Error::Unsupported(format!("PKG worker failed: {error}"));
                        let _ = sender.take().unwrap().send(Err(error.clone()));
                        Some((Err(error), (None, sender)))
                    }
                }
            },
        )
    }

    async fn finish(&mut self) -> Result<Artifact, Error> {
        let artifact = self
            .artifact
            .take()
            .unwrap()
            .with_manifest(std::mem::take(&mut self.manifest));
        self.storage.artifacts().record(&artifact).await?;
        Ok(artifact)
    }

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

struct StorageWriter {
    file: Arc<dyn StorageFile>,
    position: u64,
    hash: blake3::Hasher,
}

impl Write for StorageWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.file
            .write_all_at(self.position, bytes)
            .map_err(io::Error::other)?;
        self.position += bytes.len() as u64;
        self.hash.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush().map_err(io::Error::other)
    }
}
