use std::path::PathBuf;

use futures::Stream;
use futures::channel::oneshot;
use futures::stream;
use walkdir::WalkDir;

use crate::error::Error;
use crate::installable::DepotFile;
use crate::installable::artifacts::{Artifact, DescribeArtifact};
use crate::storage::Storage;

/// Installs a depot that is a plain host directory into a [`Storage`].
pub struct DirectoryInstall<'a> {
    /// The depot root on the host.
    pub root: PathBuf,
    /// Where the depot lands in `storage`.
    pub base: String,
    pub storage: &'a dyn Storage,
    pub artifact: &'a dyn DescribeArtifact,
}

impl<'a> DirectoryInstall<'a> {
    /// Drive the install as a stream of installed paths, handing the finished
    /// [`Artifact`] to `outcome` once the walk is exhausted.
    pub fn run(
        self,
        outcome: oneshot::Sender<Result<Artifact, Error>>,
    ) -> impl Stream<Item = Result<String, Error>> + 'a {
        let state = State {
            entries: Box::new(WalkDir::new(&self.root).into_iter()),
            install: self,
            manifest: Vec::new(),
            outcome: Some(outcome),
            done: false,
        };

        stream::unfold(state, |mut state| async move {
            if state.done {
                return None;
            }

            loop {
                let Some(entry) = state.entries.next() else {
                    let artifact = state.artifact();
                    state.report(Ok(artifact));
                    return None;
                };

                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        let path = error.path().unwrap_or(&state.install.root).to_path_buf();
                        let source = error
                            .into_io_error()
                            .unwrap_or_else(|| std::io::Error::other("directory walk failed"));

                        return Some(state.fail(Error::io(&path, source)));
                    }
                };

                let Some(relative) = entry
                    .path()
                    .strip_prefix(&state.install.root)
                    .ok()
                    .map(|path| path.to_string_lossy().replace('\\', "/"))
                    .filter(|relative| !relative.is_empty())
                else {
                    continue;
                };

                let destination = format!("{}/{relative}", state.install.base);

                if entry.file_type().is_dir() {
                    if let Err(error) = state.install.storage.create_dir_all(&destination) {
                        return Some(state.fail(error));
                    }
                    continue;
                }

                let source = match tokio::fs::File::open(entry.path()).await {
                    Ok(source) => Box::new(source),
                    Err(error) => return Some(state.fail(Error::io(entry.path(), error))),
                };

                let storage = state.install.storage;
                return match storage.install_file(source, &destination).await {
                    Ok(copied) => {
                        state.manifest.push(DepotFile::new(
                            PathBuf::from(destination),
                            copied.blake3,
                            copied.byte_length,
                        ));
                        Some((Ok(relative), state))
                    }
                    Err(error) => Some(state.fail(error)),
                };
            }
        })
    }
}

struct State<'a> {
    entries: Box<dyn Iterator<Item = walkdir::Result<walkdir::DirEntry>>>,
    install: DirectoryInstall<'a>,
    manifest: Vec<DepotFile>,
    outcome: Option<oneshot::Sender<Result<Artifact, Error>>>,
    done: bool,
}

impl State<'_> {
    fn artifact(&mut self) -> Artifact {
        let manifest = std::mem::take(&mut self.manifest);
        self.install.artifact.describe().with_manifest(manifest)
    }

    /// Terminate the stream and return the artifact.
    fn fail(mut self, error: Error) -> (Result<String, Error>, Self) {
        self.report(Err(error.clone()));
        (Err(error), self)
    }

    fn report(&mut self, outcome: Result<Artifact, Error>) {
        self.done = true;
        if let Some(sender) = self.outcome.take() {
            let _ = sender.send(outcome);
        }
    }
}
