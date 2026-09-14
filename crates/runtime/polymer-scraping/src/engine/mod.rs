mod finalize;
mod media;

use polymer_library::{
    GameCatalog, GameCatalogError, MetadataData, MetadataValue, ScrapeAttemptState, ScrapeJob,
    ScrapeJobState,
};
use polymer_pluginhost::{
    loading::PluginRegistry,
    runtime::{PluginInvocationError, PluginRuntime, PluginRuntimeHandle, PluginRuntimeOptions},
    scraper::{ScrapeCandidate, ScrapeInput, ScraperPluginInstance},
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;

// The first scrape can include plugin initialization and a metadata-index build.
// Keep it bounded without making large first-run imports spuriously fail.
const SCRAPER_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MEDIA_REQUEST_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Clone)]
pub struct ScrapingEngine {
    inner: Arc<EngineInner>,
}

#[derive(Clone, Debug)]
pub enum ScrapingEvent {
    JobChanged(ScrapeJob),
    LibraryContentChanged { root_binary_id: Uuid },
}

struct EngineInner {
    catalog: Arc<GameCatalog>,
    _runtime: Arc<PluginRuntime>,
    handle: PluginRuntimeHandle,
    client: reqwest::Client,
    events: tokio::sync::broadcast::Sender<ScrapingEvent>,
}

#[derive(Debug, thiserror::Error)]
pub enum ScrapingError {
    #[error("cannot open package path {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not identify {path}: {reason}")]
    Identify { path: PathBuf, reason: String },
    #[error("{path} was identified as unsupported platform {platform}")]
    UnsupportedPlatform { path: PathBuf, platform: String },
    #[error("unknown consolespec platform '{0}'")]
    InvalidPlatformHint(String),
    #[error(
        "Gumshoe inferred platform {inferred}, which does not match supplied platform {supplied}"
    )]
    PlatformMismatch { inferred: String, supplied: String },
    #[error(transparent)]
    Library(#[from] GameCatalogError),
    #[error(transparent)]
    Plugin(#[from] PluginInvocationError),
    #[error("media request failed: {0}")]
    Media(#[from] reqwest::Error),
    #[error("media I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid media result: {0}")]
    InvalidMedia(String),
    #[error("scraper '{plugin}' exceeded its {}-second execution limit", SCRAPER_TIMEOUT.as_secs())]
    ScraperTimeout { plugin: String },
}

impl ScrapingEngine {
    pub fn new(
        catalog: Arc<GameCatalog>,
        registry: PluginRegistry,
        directories: polymer_library::Directories,
    ) -> Self {
        let runtime = Arc::new(PluginRuntime::new(
            registry,
            directories,
            PluginRuntimeOptions {
                max_concurrent_calls: 8,
            },
        ));
        Self::with_runtime(catalog, runtime)
    }

    fn with_runtime(catalog: Arc<GameCatalog>, runtime: Arc<PluginRuntime>) -> Self {
        let handle = runtime.handle();
        let (events, _) = tokio::sync::broadcast::channel(64);
        Self {
            inner: Arc::new(EngineInner {
                catalog,
                _runtime: runtime,
                handle,
                client: reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::limited(10))
                    .timeout(MEDIA_REQUEST_TIMEOUT)
                    .build()
                    .expect("scraper HTTP client"),
                events,
            }),
        }
    }

    pub fn catalog(&self) -> &Arc<GameCatalog> {
        &self.inner.catalog
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<ScrapingEvent> {
        self.inner.events.subscribe()
    }

    pub fn publish_library_content(&self, root_binary_id: Uuid) {
        let _ = self
            .inner
            .events
            .send(ScrapingEvent::LibraryContentChanged { root_binary_id });
    }

    pub fn scrapers(&self) -> Vec<Arc<dyn ScraperPluginInstance>> {
        self.inner
            .handle
            .registry()
            .plugins::<dyn ScraperPluginInstance>()
    }

    pub async fn enqueue(&self, root: Uuid, trigger: Uuid) -> Result<ScrapeJob, ScrapingError> {
        let input = self.input(root, trigger).await?;
        let plugins = self
            .scrapers()
            .iter()
            .filter(|plugin| plugin.accepts(&input))
            .map(|plugin| plugin.id().to_owned())
            .collect::<Vec<_>>();
        let (job, created) = self
            .inner
            .catalog
            .queue_scrape(root, trigger, &plugins)
            .await?;
        if created {
            tracing::info!(job = %job.id, root = %root, scrapers = job.attempts.len(), "scrape job queued");
            let _ = self
                .inner
                .events
                .send(ScrapingEvent::JobChanged(job.clone()));
            let engine = self.clone();
            let id = job.id;
            tokio::spawn(async move {
                if let Err(error) = engine.run(id).await {
                    tracing::warn!(%error, job = %id, "scrape job failed");
                }
            });
        }
        Ok(job)
    }

    pub async fn resume(&self) -> Result<(), ScrapingError> {
        let jobs = self.inner.catalog.recover_scrape_jobs().await?;
        let active: std::collections::BTreeSet<_> =
            jobs.iter().map(|job| job.root_binary_id).collect();
        let pending: std::collections::BTreeSet<_> = self
            .inner
            .catalog
            .pending_manifests(None)
            .await?
            .into_iter()
            .map(|(_, root, _)| root)
            .collect();
        for root in pending.difference(&active) {
            self.finalize_packages(Some(*root)).await?;
        }
        for job in jobs {
            let engine = self.clone();
            tokio::spawn(async move {
                if let Err(error) = engine.run(job.id).await {
                    tracing::warn!(%error, job = %job.id, "recovered scrape job failed");
                }
            });
        }
        Ok(())
    }

    pub async fn retry(&self, id: Uuid) -> Result<ScrapeJob, ScrapingError> {
        let job = self
            .inner
            .catalog
            .scrape_job(id)
            .await?
            .ok_or_else(|| GameCatalogError::NotFound(id.simple().to_string()))?;
        self.enqueue(job.root_binary_id, job.trigger_binary_id)
            .await
    }

    pub async fn refresh(&self, plugin_id: &str) -> Result<(), ScrapingError> {
        if self
            .inner
            .handle
            .registry()
            .plugin::<dyn ScraperPluginInstance>(plugin_id)
            .is_none()
        {
            return Err(PluginInvocationError::new(
                "NotFoundError",
                "UNKNOWN_SCRAPER",
                format!("unknown scraper '{plugin_id}'"),
            )
            .into());
        }
        self.inner
            .handle
            .invoke(plugin_id, &["updateContents".to_owned()], Vec::new())
            .await?;
        Ok(())
    }

    async fn run(&self, id: Uuid) -> Result<(), ScrapingError> {
        if !self.inner.catalog.claim_scrape_job(id).await? {
            return Ok(());
        }
        self.publish_job(id).await;
        let outcome = self.execute_job(id).await;
        let state = match &outcome {
            Ok(state) => *state,
            Err(error) => {
                tracing::warn!(%error, job = %id, "scrape job execution failed");
                ScrapeJobState::Failed
            }
        };
        self.inner.catalog.set_scrape_job_state(id, state).await?;
        let job = self.inner.catalog.scrape_job(id).await?;
        if let Some(job) = job {
            self.finalize_packages(Some(job.root_binary_id)).await?;
        }
        self.publish_job(id).await;
        outcome.map(|_| ())
    }

    async fn execute_job(&self, id: Uuid) -> Result<ScrapeJobState, ScrapingError> {
        let job = self
            .inner
            .catalog
            .scrape_job(id)
            .await?
            .ok_or_else(|| GameCatalogError::NotFound(id.simple().to_string()))?;
        let input = self
            .input(job.root_binary_id, job.trigger_binary_id)
            .await?;
        let mut tasks = tokio::task::JoinSet::new();
        let already_completed = job
            .attempts
            .iter()
            .filter(|attempt| attempt.state == ScrapeAttemptState::Completed)
            .count();
        for attempt in job
            .attempts
            .into_iter()
            .filter(|attempt| attempt.state != ScrapeAttemptState::Completed)
        {
            let engine = self.clone();
            let input = input.clone();
            tasks.spawn(async move {
                let plugin = attempt.plugin_id;
                engine
                    .inner
                    .catalog
                    .set_scrape_attempt(id, &plugin, ScrapeAttemptState::Running, None)
                    .await?;
                tracing::info!(job = %id, scraper = %plugin, "scraper started");
                engine.publish_job(id).await;
                let result = engine.run_plugin(job.root_binary_id, &plugin, input).await;
                match &result {
                    Ok(()) => {
                        tracing::info!(job = %id, scraper = %plugin, "scraper completed");
                        engine
                            .inner
                            .catalog
                            .set_scrape_attempt(id, &plugin, ScrapeAttemptState::Completed, None)
                            .await?
                    }
                    Err(error) => {
                        tracing::warn!(job = %id, scraper = %plugin, %error, "scraper failed");
                        engine
                            .inner
                            .catalog
                            .set_scrape_attempt(
                                id,
                                &plugin,
                                ScrapeAttemptState::Failed,
                                Some(&error.to_string()),
                            )
                            .await?
                    }
                }
                engine.publish_job(id).await;
                Ok::<bool, ScrapingError>(result.is_ok())
            });
        }
        let mut completed = already_completed;
        let mut failed = 0usize;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(true)) => completed += 1,
                _ => failed += 1,
            }
        }
        Ok(match (completed, failed) {
            (_, 0) => ScrapeJobState::Completed,
            (0, _) => ScrapeJobState::Failed,
            _ => ScrapeJobState::Partial,
        })
    }

    async fn publish_job(&self, id: Uuid) {
        match self.inner.catalog.scrape_job(id).await {
            Ok(Some(job)) => {
                let finished = job
                    .attempts
                    .iter()
                    .filter(|attempt| {
                        matches!(
                            attempt.state,
                            ScrapeAttemptState::Completed | ScrapeAttemptState::Failed
                        )
                    })
                    .count();
                tracing::info!(job = %id, state = ?job.state, finished, total = job.attempts.len(), "scrape job progress");
                let _ = self.inner.events.send(ScrapingEvent::JobChanged(job));
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, job = %id, "could not publish scrape job"),
        }
    }

    async fn input(&self, root: Uuid, trigger: Uuid) -> Result<ScrapeInput, ScrapingError> {
        let mut input = BTreeMap::<String, Vec<ScrapeCandidate>>::new();
        let mut metadata = self.inner.catalog.metadata(root).await?;
        if trigger != root {
            metadata.extend(self.inner.catalog.metadata(trigger).await?);
        }
        for value in metadata {
            let scalar = match value.data {
                MetadataData::Scalar(value) => value,
                MetadataData::Media {
                    source_url: Some(value),
                    ..
                } => MetadataValue::String(value),
                MetadataData::Media {
                    source_url: None, ..
                } => continue,
            };
            input.entry(value.key).or_default().push(ScrapeCandidate {
                source: value.source,
                value: scalar,
            });
        }
        Ok(input)
    }

    async fn run_plugin(
        &self,
        root: Uuid,
        plugin: &str,
        input: ScrapeInput,
    ) -> Result<(), ScrapingError> {
        let definition = self
            .inner
            .handle
            .registry()
            .plugin::<dyn ScraperPluginInstance>(plugin)
            .ok_or_else(|| {
                PluginInvocationError::new(
                    "NotFoundError",
                    "UNKNOWN_SCRAPER",
                    format!("unknown scraper '{plugin}'"),
                )
            })?;
        let input = definition.requested_input(&input);
        let cancellation = self.inner.handle.cancellation_token();
        let invocation = self
            .inner
            .handle
            .scrape(plugin, input, cancellation.clone());
        let results = match tokio::time::timeout(SCRAPER_TIMEOUT, invocation).await {
            Ok(result) => result?,
            Err(_) => {
                cancellation.cancel();
                return Err(ScrapingError::ScraperTimeout {
                    plugin: plugin.to_owned(),
                });
            }
        };
        let temporary = tempfile::tempdir()?;
        let inputs = self.stage_media(results, temporary.path()).await?;
        self.inner
            .catalog
            .replace_metadata(root, &format!("scraper:{plugin}"), inputs)
            .await?;
        self.publish_library_content(root);
        Ok(())
    }
}
