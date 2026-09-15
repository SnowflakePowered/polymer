use super::{Depot, DepotFile, artifacts::Artifact};
use crate::Error;
use crate::nand::VirtualNand;
use futures::channel::oneshot;
use futures::{Stream, StreamExt, stream};
use std::pin::Pin;
use std::task::{Context, Poll};

/// Archive-specific extraction and completion for an installation.
#[allow(async_fn_in_trait)]
pub trait Installable: Sized {
    /// Describe the artifact, including any files retained from a previous install.
    fn artifact(&self) -> &Artifact;
    /// Install the next batch. `None` means extraction is complete.
    async fn next_batch(&mut self) -> Result<Option<Vec<DepotFile>>, Error>;
    /// Complete a successfully extracted installation, recording it when required.
    async fn finish(self, artifact: Artifact) -> Result<Artifact, Error>;
}

/// Installation progress driven directly by `next().await`.
/// Use `finish().await` to drain remaining progress and obtain the final artifact.
pub struct Installation<'a> {
    progress: Pin<Box<dyn Stream<Item = Result<String, Error>> + 'a>>,
    outcome: Pin<Box<dyn Future<Output = Result<Artifact, Error>> + 'a>>,
}
impl<'a> Installation<'a> {
    pub fn new<D: Depot + 'a, N: VirtualNand + ?Sized>(depot: D, nand: &'a N) -> Self {
        depot.install(nand)
    }

    pub(crate) fn from_installable<I: Installable + 'a>(installable: I) -> Self {
        Self::prepare(std::future::ready(Ok(installable)))
    }

    /// Lazily prepare an archive when progress or completion is polled.
    pub(crate) fn prepare<I, F>(prepare: F) -> Self
    where
        I: Installable + 'a,
        F: Future<Output = Result<I, Error>> + 'a,
    {
        let (progress, outcome) = Self::drive(prepare);
        Self::from_progress(progress, outcome)
    }

    pub(crate) fn from_progress<S, F>(progress: S, outcome: F) -> Self
    where
        S: Stream<Item = Result<String, Error>> + 'a,
        F: Future<Output = Result<Artifact, Error>> + 'a,
    {
        Self {
            progress: Box::pin(progress.fuse()),
            outcome: Box::pin(outcome),
        }
    }

    /// Consume remaining progress and return the installation result.
    /// Also returns an error previously observed through `next()`.
    pub async fn finish(mut self) -> Result<Artifact, Error> {
        while self.next().await.is_some() {}
        self.outcome.await
    }

    fn drive<I, F>(
        prepare: F,
    ) -> (
        impl Stream<Item = Result<String, Error>> + 'a,
        impl Future<Output = Result<Artifact, Error>> + 'a,
    )
    where
        I: Installable + 'a,
        F: Future<Output = Result<I, Error>> + 'a,
    {
        let (sender, outcome) = oneshot::channel();
        let progress = stream::once(async move {
            let installable = match prepare.await {
                Ok(installable) => installable,
                Err(error) => {
                    let _ = sender.send(Err(error.clone()));
                    return futures::future::Either::Left(stream::once(async { Err(error) }));
                }
            };
            let artifact = installable.artifact().clone();
            let manifest = artifact.manifest().to_vec();
            let state = Some((
                installable,
                artifact,
                manifest,
                std::collections::VecDeque::<DepotFile>::new(),
                sender,
            ));
            let progress = stream::unfold(state, |state| async move {
                let (mut installable, artifact, mut manifest, mut pending, sender) = state?;
                let result = loop {
                    if let Some(file) = pending.pop_front() {
                        let path = file.path().to_string_lossy().into_owned();
                        manifest.retain(|old| old.path() != file.path());
                        manifest.push(file);
                        return Some((
                            Ok(path),
                            Some((installable, artifact, manifest, pending, sender)),
                        ));
                    }
                    match installable.next_batch().await {
                        Ok(Some(files)) => {
                            pending.extend(files);
                        }
                        Ok(None) => {
                            break installable.finish(artifact.with_manifest(manifest)).await;
                        }
                        Err(error) => break Err(error),
                    }
                };
                let error = result.as_ref().err().cloned();
                let _ = sender.send(result);
                error.map(|error| (Err(error), None))
            });
            futures::future::Either::Right(progress)
        })
        .flatten();
        (progress, async move {
            outcome.await.unwrap_or(Err(Error::InstallIncomplete))
        })
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
    use std::collections::VecDeque;

    struct Archive<'a> {
        artifact: Artifact,
        batches: VecDeque<Result<Vec<DepotFile>, Error>>,
        finished: &'a Cell<bool>,
        wait: Option<oneshot::Receiver<()>>,
        fail_finish: bool,
    }
    impl Installable for Archive<'_> {
        fn artifact(&self) -> &Artifact {
            &self.artifact
        }
        async fn next_batch(&mut self) -> Result<Option<Vec<DepotFile>>, Error> {
            self.batches.pop_front().transpose()
        }
        async fn finish(self, artifact: Artifact) -> Result<Artifact, Error> {
            if let Some(wait) = self.wait {
                wait.await.unwrap();
            }
            self.finished.set(true);
            if self.fail_finish {
                return Err(Error::Unsupported("finish failed".into()));
            }
            Ok(artifact)
        }
    }

    #[tokio::test]
    async fn next_drives_batches_and_pending_completion() {
        let finished = Cell::new(false);
        let (release, wait) = oneshot::channel();
        let mut install = Installation::from_installable(Archive {
            artifact: Artifact::new(
                "test".into(),
                DepotType::App,
                Vec::new(),
                "Test".into(),
                None,
                IconHint::Builtin(DepotType::App),
            ),
            batches: [
                Ok(vec![]),
                Ok(vec![
                    DepotFile::new("file".into(), [1; 32], 1),
                    DepotFile::new("file".into(), [2; 32], 2),
                ]),
            ]
            .into(),
            finished: &finished,
            wait: Some(wait),
            fail_finish: false,
        });
        assert_eq!(install.next().await.unwrap().unwrap(), "file");
        assert_eq!(install.next().await.unwrap().unwrap(), "file");
        assert!(install.next().now_or_never().is_none());
        assert!(!finished.get());
        release.send(()).unwrap();
        assert!(install.next().await.is_none());
        let artifact = install.finish().await.unwrap();
        assert_eq!(artifact.manifest().len(), 1);
        assert_eq!(artifact.manifest()[0].blake3(), &[2; 32]);
        assert!(finished.get());
    }

    #[tokio::test]
    async fn extraction_and_completion_errors_reach_both_outputs() {
        for fail_finish in [false, true] {
            let finished = Cell::new(false);
            let batches = if fail_finish {
                VecDeque::new()
            } else {
                [Err(Error::Unsupported("extraction failed".into()))].into()
            };
            let mut install = Installation::from_installable(Archive {
                artifact: Artifact::new(
                    "test".into(),
                    DepotType::App,
                    Vec::new(),
                    "Test".into(),
                    None,
                    IconHint::Builtin(DepotType::App),
                ),
                batches,
                finished: &finished,
                wait: None,
                fail_finish,
            });
            let error = install.next().await.unwrap().unwrap_err();
            assert_eq!(
                error.to_string(),
                install.finish().await.err().unwrap().to_string()
            );
            assert_eq!(finished.get(), fail_finish);
        }
    }

    #[tokio::test]
    async fn dropping_progress_does_not_start_completion() {
        let finished = Cell::new(false);
        let mut install = Installation::from_installable(Archive {
            artifact: Artifact::new(
                "test".into(),
                DepotType::App,
                Vec::new(),
                "Test".into(),
                None,
                IconHint::Builtin(DepotType::App),
            ),
            batches: [Ok(vec![DepotFile::new("file".into(), [0; 32], 0)])].into(),
            finished: &finished,
            wait: None,
            fail_finish: false,
        });
        install.next().await.unwrap().unwrap();
        drop(install);
        assert!(!finished.get());
    }

    #[tokio::test]
    async fn finish_drains_unpolled_or_partially_consumed_installation() {
        for consume_progress in [false, true] {
            let finished = Cell::new(false);
            let mut install = Installation::from_installable(Archive {
                artifact: Artifact::new(
                    "test".into(),
                    DepotType::App,
                    Vec::new(),
                    "Test".into(),
                    None,
                    IconHint::Builtin(DepotType::App),
                ),
                batches: [Ok(vec![
                    DepotFile::new("first".into(), [1; 32], 1),
                    DepotFile::new("second".into(), [2; 32], 2),
                ])]
                .into(),
                finished: &finished,
                wait: None,
                fail_finish: false,
            });
            if consume_progress {
                assert_eq!(install.next().await.unwrap().unwrap(), "first");
            }
            let artifact = install.finish().await.unwrap();
            assert_eq!(artifact.manifest().len(), 2);
            assert!(finished.get());
        }
    }

    #[tokio::test]
    async fn preparation_error_is_retained_after_progress_reports_it() {
        let mut install = Installation::prepare(async {
            Err::<Archive<'static>, _>(Error::Unsupported("cannot open archive".into()))
        });
        let error = install.next().await.unwrap().unwrap_err();
        assert!(install.next().await.is_none());
        assert_eq!(
            install.finish().await.err().unwrap().to_string(),
            error.to_string()
        );
    }
}
