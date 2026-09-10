use super::{DepotFile, artifacts::Artifact};
use crate::Error;
use futures::channel::oneshot;
use futures::{Stream, StreamExt, stream};
use std::pin::Pin;
use std::task::{Context, Poll};

/// A file installation driven by polling progress, including finalization.
/// Completion is driven by the stream, not by awaiting the outcome receiver.
pub struct Installation<'a> {
    progress: Pin<Box<dyn Stream<Item = Result<String, Error>> + 'a>>,
}

impl<'a> Installation<'a> {
    /// Wrap installed files with manifest collection and a completion callback.
    /// Existing manifest entries are retained unless a new file replaces their path.
    /// The callback runs only after the source is exhausted successfully. Its
    /// result is sent to `outcome`; failures also appear as the last stream item.
    pub fn new<S, F, Fut>(
        files: S,
        artifact: Artifact,
        finish: F,
        outcome: oneshot::Sender<Result<Artifact, Error>>,
    ) -> Self
    where
        S: Stream<Item = Result<DepotFile, Error>> + 'a,
        F: FnOnce(Artifact) -> Fut + 'a,
        Fut: Future<Output = Result<Artifact, Error>> + 'a,
    {
        let manifest = artifact
            .manifest()
            .iter()
            .map(|file| DepotFile::new(file.path().to_owned(), *file.blake3(), file.byte_length()))
            .collect::<Vec<_>>();
        let state = Some((Box::pin(files), artifact, manifest, finish, outcome));
        let progress = stream::unfold(state, |state| async move {
            let (mut files, artifact, mut manifest, finish, sender) = state?;
            let result = match files.next().await {
                Some(Ok(file)) => {
                    let path = file.path().to_string_lossy().into_owned();
                    manifest.retain(|old| old.path() != file.path());
                    manifest.push(file);
                    return Some((Ok(path), Some((files, artifact, manifest, finish, sender))));
                }
                Some(Err(error)) => Err(error),
                None => finish(artifact.with_manifest(manifest)).await,
            };
            // No further polling or writes after a terminal result.
            let error = result.as_ref().err().cloned();
            let _ = sender.send(result);
            error.map(|error| (Err(error), None))
        });
        Self {
            progress: Box::pin(progress.fuse()),
        }
    }
}

impl Stream for Installation<'_> {
    type Item = Result<String, Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.progress.as_mut().poll_next(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::installable::{DepotType, IconHint};
    use futures::FutureExt;
    use std::cell::Cell;
    use std::path::PathBuf;

    struct Fixture;
    impl Fixture {
        fn artifact() -> Artifact {
            Artifact::new(
                "test".into(),
                DepotType::App,
                Vec::new(),
                "Test".into(),
                None,
                IconHint::Builtin(DepotType::App),
            )
        }
        fn file(path: &str, data: &[u8]) -> DepotFile {
            DepotFile::new(
                PathBuf::from(path),
                *blake3::hash(data).as_bytes(),
                data.len() as u64,
            )
        }
    }

    impl super::super::artifacts::DescribeArtifact for Fixture {
        fn describe(&self) -> Artifact {
            Self::artifact()
        }
    }

    #[tokio::test]
    async fn directory_stream_copies_nested_files_and_reports_destination_paths() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("nested/empty")).unwrap();
        std::fs::write(source.path().join("nested/data"), b"payload").unwrap();
        let storage = std::sync::Arc::new(
            crate::storage::DirectoryStorage::new(destination.path())
                .await
                .unwrap(),
        );
        let (sender, outcome) = oneshot::channel();
        let description = Fixture;
        let mut install = super::super::DirectoryInstall {
            root: source.path().to_owned(),
            base: "game".into(),
            storage,
            artifact: &description,
        }
        .run(sender);
        assert_eq!(install.next().await.unwrap().unwrap(), "game/nested/data");
        assert!(install.next().await.is_none());
        let artifact = outcome.await.unwrap().unwrap();
        assert_eq!(artifact.manifest().len(), 1);
        assert_eq!(
            artifact.manifest()[0].blake3(),
            blake3::hash(b"payload").as_bytes()
        );
        assert_eq!(
            std::fs::read(destination.path().join("game/nested/data")).unwrap(),
            b"payload"
        );
        assert!(destination.path().join("game/nested/empty").is_dir());
    }

    #[tokio::test]
    async fn next_drives_pending_finalization_and_merges_manifest() {
        let artifact = Fixture::artifact().with_manifest(vec![
            Fixture::file("retained", b"old"),
            Fixture::file("replaced", b"old"),
        ]);
        let (sender, mut outcome) = oneshot::channel();
        let (release, wait) = oneshot::channel();
        let finished = Cell::new(false);
        let mut install = Installation::new(
            stream::iter([Ok(Fixture::file("replaced", b"new"))]),
            artifact,
            |artifact| async {
                wait.await.unwrap();
                finished.set(true);
                Ok(artifact)
            },
            sender,
        );
        assert_eq!(install.next().await.unwrap().unwrap(), "replaced");
        assert!(outcome.try_recv().unwrap().is_none());
        assert!(install.next().now_or_never().is_none());
        assert!(!finished.get());
        release.send(()).unwrap();
        assert!(install.next().await.is_none());
        let artifact = outcome.await.unwrap().unwrap();
        assert!(finished.get());
        assert_eq!(artifact.manifest().len(), 2);
        assert_eq!(
            artifact.manifest()[1].blake3(),
            blake3::hash(b"new").as_bytes()
        );
        assert!(install.next().await.is_none());
    }

    #[tokio::test]
    async fn source_error_skips_finalization_and_is_terminal() {
        let (sender, outcome) = oneshot::channel();
        let finished = Cell::new(false);
        let files = stream::iter([
            Err(Error::Unsupported("source failed".into())),
            Ok(Fixture::file("unreachable", b"x")),
        ]);
        let mut install = Installation::new(
            files,
            Fixture::artifact(),
            |artifact| async {
                finished.set(true);
                Ok(artifact)
            },
            sender,
        );
        let error = install.next().await.unwrap().unwrap_err();
        assert_eq!(
            error.to_string(),
            outcome.await.unwrap().err().unwrap().to_string()
        );
        assert!(install.next().await.is_none());
        assert!(!finished.get());
    }

    #[tokio::test]
    async fn finalization_error_is_reported_and_drop_does_not_finalize() {
        let (sender, outcome) = oneshot::channel();
        let mut install = Installation::new(
            stream::empty(),
            Fixture::artifact(),
            |_| async { Err(Error::Unsupported("commit failed".into())) },
            sender,
        );
        let error = install.next().await.unwrap().unwrap_err();
        assert_eq!(
            error.to_string(),
            outcome.await.unwrap().err().unwrap().to_string()
        );
        assert!(install.next().await.is_none());

        let (sender, outcome) = oneshot::channel();
        let finished = Cell::new(false);
        let mut install = Installation::new(
            stream::iter([Ok(Fixture::file("partial", b"x"))]),
            Fixture::artifact(),
            |artifact| async {
                finished.set(true);
                Ok(artifact)
            },
            sender,
        );
        install.next().await.unwrap().unwrap();
        drop(install);
        assert!(outcome.await.is_err());
        assert!(!finished.get());
    }
}
