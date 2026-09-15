use crate::Error;
use crate::installable::artifacts::{Artifact, DescribeArtifact};
use crate::installable::{DepotFile, Installable};
use crate::storage::Storage;
use std::path::PathBuf;
use std::sync::Arc;
use walkdir::WalkDir;

/// Installs a depot that is a plain host directory into a [`Storage`].
pub struct DirectoryInstall {
    /// The depot root on the host.
    pub root: PathBuf,
    /// Where the depot lands in `storage`.
    pub base: String,
    pub storage: Arc<dyn Storage>,
    entries: walkdir::IntoIter,
    pub artifact: Artifact,
}

impl DirectoryInstall {
    pub fn new(
        root: PathBuf,
        base: String,
        storage: Arc<dyn Storage>,
        artifact: &dyn DescribeArtifact,
    ) -> Self {
        let entries = WalkDir::new(&root).into_iter();
        Self {
            root,
            base,
            storage,
            artifact: artifact.describe(),
            entries,
        }
    }
}
impl Installable for DirectoryInstall {
    fn artifact(&self) -> &Artifact {
        &self.artifact
    }
    async fn next_batch(&mut self) -> Result<Option<Vec<DepotFile>>, Error> {
        for entry in self.entries.by_ref() {
            let entry = entry.map_err(|error| {
                let path = error.path().unwrap_or(&self.root).to_owned();
                let source = error
                    .into_io_error()
                    .unwrap_or_else(|| std::io::Error::other("directory walk failed"));
                Error::io(&path, source)
            })?;
            let relative = entry
                .path()
                .strip_prefix(&self.root)
                .map_err(|_| Error::InvalidPath(entry.path().display().to_string()))?
                .to_string_lossy()
                .replace('\\', "/");
            if relative.is_empty() {
                continue;
            }
            let destination = if self.base.is_empty() {
                relative
            } else {
                format!("{}/{relative}", self.base)
            };
            if entry.file_type().is_symlink() {
                return Err(Error::InvalidPath(destination));
            }
            if entry.file_type().is_dir() {
                self.storage.create_dir_all(&destination)?;
                continue;
            }
            let source = tokio::fs::File::open(entry.path())
                .await
                .map_err(|error| Error::io(entry.path(), error))?;
            let copied = self
                .storage
                .install_file(Box::new(source), &destination)
                .await?;
            let file = DepotFile::new(
                PathBuf::from(destination),
                copied.blake3,
                copied.byte_length,
            );
            return Ok(Some(vec![file]));
        }
        Ok(None)
    }
    async fn finish(self, artifact: Artifact) -> Result<Artifact, Error> {
        Ok(artifact)
    }
}
