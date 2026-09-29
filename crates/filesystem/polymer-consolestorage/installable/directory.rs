use crate::Error;
use crate::installable::artifacts::Artifact;
use crate::installable::{DepotFile, InstallEvents, InstallPhase, Installable, Reporting};
use crate::storage::Storage;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use walkdir::WalkDir;

/// Installs a depot that is a plain host directory into a [`Storage`], then
/// builds its artifact with `finish`.
pub struct DirectoryInstall<F> {
    /// The depot root on the host.
    pub root: PathBuf,
    /// Where the depot lands in `storage`.
    pub base: String,
    pub storage: Arc<dyn Storage>,
    entries: walkdir::IntoIter,
    excluded: BTreeSet<String>,
    events: InstallEvents,
    finish: F,
}

impl<F: FnOnce(Vec<DepotFile>) -> Result<Artifact, Error>> DirectoryInstall<F> {
    /// Publishes the depot's total size as the installing phase.
    pub fn new(
        root: PathBuf,
        base: String,
        storage: Arc<dyn Storage>,
        events: InstallEvents,
        finish: F,
    ) -> Self {
        let total = WalkDir::new(&root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter_map(|entry| entry.metadata().ok())
            .map(|metadata| metadata.len())
            .sum();
        events.phase(InstallPhase::Installing, total);
        let entries = WalkDir::new(&root).into_iter();
        Self {
            root,
            base,
            storage,
            entries,
            excluded: BTreeSet::new(),
            events,
            finish,
        }
    }

    pub fn excluding(mut self, relative: impl Into<String>) -> Self {
        self.excluded.insert(relative.into());
        self
    }
}

impl<F: FnOnce(Vec<DepotFile>) -> Result<Artifact, Error>> Installable for DirectoryInstall<F> {
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
            if self.excluded.contains(&relative) {
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
                .install_file(
                    Box::new(Reporting {
                        inner: source,
                        events: self.events.clone(),
                    }),
                    &destination,
                )
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

    async fn finish(self, manifest: Vec<DepotFile>) -> Result<Artifact, Error> {
        (self.finish)(manifest)
    }
}
