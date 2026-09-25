use super::{Depot, DepotFile, InstallEvent, InstallEvents, artifacts::Artifact};
use crate::Error;
use crate::nand::VirtualNand;
use futures::channel::oneshot;
use futures::{Stream, StreamExt, stream};
use std::pin::Pin;
use std::task::{Context, Poll};

/// Archive-specific extraction and completion for an installation.
#[allow(async_fn_in_trait)]
pub trait Installable: Sized {
    /// Install the next batch. `None` means extraction is complete.
    async fn next_batch(&mut self) -> Result<Option<Vec<DepotFile>>, Error>;
    /// Reconcile retained files and read installed metadata, then build and record the artifact.
    async fn finish(self, manifest: Vec<DepotFile>) -> Result<Artifact, Error>;
}

/// Installation progress driven directly by `next().await`.
/// Use `finish().await` to drain remaining progress and obtain the final artifact.
pub struct Installation<'a> {
    keys: Vec<crate::keys::KeyRecord>,
    progress: Pin<Box<dyn Stream<Item = Result<String, Error>> + 'a>>,
    outcome: Pin<Box<dyn Future<Output = Result<Artifact, Error>> + 'a>>,
}
impl<'a> Installation<'a> {
    pub fn new<D: Depot + 'a, N: VirtualNand + ?Sized>(
        depot: D,
        nand: &'a N,
        events: InstallEvents,
    ) -> Result<Self, Error> {
        let keys = depot.embedded_keys()?;
        if let Some(title) = depot.preview() {
            events(InstallEvent::Title(title));
        }
        let mut installation = depot.install(nand, events);
        installation.keys = keys;
        Ok(installation)
    }

    /// Source keys for the host to persist before polling installation progress.
    pub fn embedded_keys(&self) -> &[crate::keys::KeyRecord] {
        &self.keys
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
            keys: Vec::new(),
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
            let manifest = std::collections::BTreeMap::<std::path::PathBuf, DepotFile>::new();
            let state = Some((
                installable,
                manifest,
                std::collections::VecDeque::<DepotFile>::new(),
                sender,
            ));
            let progress = stream::unfold(state, |state| async move {
                let (mut installable, mut manifest, mut pending, sender) = state?;
                let result = loop {
                    if let Some(file) = pending.pop_front() {
                        let path = file.path().to_string_lossy().into_owned();
                        manifest.insert(file.path().to_owned(), file);
                        return Some((Ok(path), Some((installable, manifest, pending, sender))));
                    }
                    match installable.next_batch().await {
                        Ok(Some(files)) => {
                            pending.extend(files);
                        }
                        Ok(None) => {
                            break installable.finish(manifest.into_values().collect()).await;
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
