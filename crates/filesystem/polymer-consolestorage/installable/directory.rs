use crate::Error;
use crate::installable::artifacts::{Artifact, DescribeArtifact};
use crate::installable::{DepotFile, Installation};
use crate::storage::Storage;
use futures::channel::oneshot;
use futures::stream;
use std::path::PathBuf;
use std::sync::Arc;
use walkdir::WalkDir;

/// Installs a depot that is a plain host directory into a [`Storage`].
pub struct DirectoryInstall<'a> {
    /// The depot root on the host.
    pub root: PathBuf,
    /// Where the depot lands in `storage`.
    pub base: String,
    pub storage: Arc<dyn Storage>,
    pub artifact: &'a dyn DescribeArtifact,
}

impl<'a> DirectoryInstall<'a> {
    /// Drive copying and manifest collection through `next().await`.
    pub fn run(self, outcome: oneshot::Sender<Result<Artifact, Error>>) -> Installation<'a> {
        let artifact = self.artifact.describe();
        let entries = WalkDir::new(&self.root).into_iter();
        let files = stream::try_unfold((self, entries), |(install, mut entries)| async move {
            for entry in entries.by_ref() {
                let entry = entry.map_err(|error| {
                    let path = error.path().unwrap_or(&install.root).to_owned();
                    let source = error
                        .into_io_error()
                        .unwrap_or_else(|| std::io::Error::other("directory walk failed"));
                    Error::io(&path, source)
                })?;
                let relative = entry
                    .path()
                    .strip_prefix(&install.root)
                    .map_err(|_| Error::InvalidPath(entry.path().display().to_string()))?
                    .to_string_lossy()
                    .replace('\\', "/");
                if relative.is_empty() {
                    continue;
                }
                let destination = format!("{}/{relative}", install.base);
                if entry.file_type().is_dir() {
                    install.storage.create_dir_all(&destination)?;
                    continue;
                }
                let source = tokio::fs::File::open(entry.path())
                    .await
                    .map_err(|error| Error::io(entry.path(), error))?;
                let copied = install
                    .storage
                    .install_file(Box::new(source), &destination)
                    .await?;
                let file = DepotFile::new(
                    PathBuf::from(destination),
                    copied.blake3,
                    copied.byte_length,
                );
                return Ok(Some((file, (install, entries))));
            }
            Ok(None)
        });
        Installation::new(files, artifact, |artifact| async { Ok(artifact) }, outcome)
    }
}
