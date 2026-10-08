use super::progress::ProgressReporter;
use super::{Depot, DepotFile, InstallContext, InstallEvent, artifacts::Artifact};
use crate::Error;
use crate::nand::VirtualNand;
use genawaiter::{
    GeneratorState,
    rc::{Co, Gen},
};
use std::collections::BTreeMap;
use std::path::Path;
use std::pin::Pin;

type Outcome<'a> = Pin<Box<dyn Future<Output = Result<Artifact, Error>> + 'a>>;

/// Archive-specific extraction and completion for an installation.
#[allow(async_fn_in_trait)]
pub trait Installable: Sized {
    /// Install the next batch, reporting through `context`. `None` means extraction is complete.
    async fn next_batch(
        &mut self,
        context: &InstallContext,
    ) -> Result<Option<Vec<DepotFile>>, Error>;
    /// Reconcile retained files and read installed metadata, then build the artifact.
    async fn finish(self, manifest: Vec<DepotFile>) -> Result<Artifact, Error>;
}

/// Installation progress driven directly by `next().await`.
/// Use `finish().await` to drain remaining progress and obtain the final artifact.
pub struct Installation<'a> {
    keys: Vec<crate::keys::KeyRecord>,
    progress: Gen<InstallEvent, (), Outcome<'a>>,
    outcome: Option<Result<Artifact, Error>>,
}
impl<'a> Installation<'a> {
    pub fn new<D: Depot, N: VirtualNand + ?Sized>(
        depot: D,
        path: &Path,
        nand: &'a N,
        store: &'a dyn crate::keys::KeyReader,
    ) -> Result<Self, Error> {
        let keys = depot.embedded_keys()?;
        let root = path.display().to_string();
        let progress = Gen::new(move |co| -> Outcome<'a> {
            Box::pin(Self::drive(co, depot, root, nand, store))
        });
        Ok(Self {
            keys,
            progress,
            outcome: None,
        })
    }

    /// Source keys for the host to persist before polling installation progress.
    pub fn embedded_keys(&self) -> &[crate::keys::KeyRecord] {
        &self.keys
    }

    /// The next progress event, or `None` once the installation has ended.
    /// An error ends the installation and is also returned by `finish()`.
    pub async fn next(&mut self) -> Option<Result<InstallEvent, Error>> {
        if self.outcome.is_some() {
            return None;
        }
        match self.progress.async_resume().await {
            GeneratorState::Yielded(event) => Some(Ok(event)),
            GeneratorState::Complete(outcome) => {
                let error = outcome.as_ref().err().cloned();
                self.outcome = Some(outcome);
                error.map(Err)
            }
        }
    }

    /// Consume remaining progress and return the installation result.
    /// Also returns an error previously observed through `next()`.
    pub async fn finish(mut self) -> Result<Artifact, Error> {
        while self.next().await.is_some() {}
        self.outcome.unwrap_or(Err(Error::InstallIncomplete))
    }

    /// Yields the title, then validates and extracts each batch, yielding progress as it
    /// is reported and every installed file, then records the artifact.
    async fn drive<D: Depot, N: VirtualNand + ?Sized>(
        co: Co<InstallEvent>,
        depot: D,
        root: String,
        nand: &N,
        store: &dyn crate::keys::KeyReader,
    ) -> Result<Artifact, Error> {
        let (context, changes) = InstallContext::new();
        let mut reporter = ProgressReporter::new(co, changes);
        if let Some(title) = depot.preview() {
            reporter.emit(InstallEvent::Title(title)).await;
        }
        let prepare = Self::prepare(depot, root, nand, store, &context);
        let mut installable = reporter.run(prepare).await?;
        let mut manifest = BTreeMap::new();
        while let Some(files) = reporter.run(installable.next_batch(&context)).await? {
            for file in files {
                let path = file.path().to_owned();
                manifest.insert(path.clone(), file);
                reporter.emit(InstallEvent::File(path)).await;
            }
        }
        let manifest = manifest.into_values().collect();
        let artifact = reporter.run(installable.finish(manifest)).await?;
        let storage = nand.root();
        let artifacts = storage.artifacts();
        reporter.run(artifacts.record(&artifact, &context)).await?;
        Ok(artifact)
    }

    /// Checks the NAND and the depot's contents, then prepares extraction.
    async fn prepare<D: Depot, N: VirtualNand + ?Sized>(
        depot: D,
        root: String,
        nand: &N,
        store: &dyn crate::keys::KeyReader,
        context: &InstallContext,
    ) -> Result<impl Installable, Error> {
        if nand.is_dirty()? {
            return Err(Error::HistoryConflict(
                "This NAND has uncommitted changes. Recover it in Console storage before installing.".into(),
            ));
        }
        depot.satisfies(nand)?;
        let (depot, missing) = context
            .spawn_blocking(move |context| {
                let missing = depot.validate_contents(context);
                (depot, missing)
            })
            .await
            .map_err(|error| Error::Unsupported(format!("depot worker failed: {error}")))?;
        if !missing.is_empty() {
            return Err(Error::InvalidDepot { root, missing });
        }
        depot.install(nand, store, context).await
    }
}
