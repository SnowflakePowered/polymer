use futures_util::{StreamExt, TryStreamExt};
use snowglass_library::{
    GameCatalog, GameCatalogError, MetadataData, MetadataInput, MetadataInputData, MetadataValue,
    ScrapeAttemptState, ScrapeJob, ScrapeJobState,
};
use snowglass_pluginhost::{
    PluginInvocationError, PluginRegistry, PluginRuntime, PluginRuntimeHandle,
    PluginRuntimeOptions, ScrapeCandidate, ScrapeInput, ScraperPluginInstance,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

const MAX_MEDIA_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ATTEMPT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_CONCURRENT_MEDIA_DOWNLOADS: usize = 4;
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
        directories: snowglass_library::Directories,
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
        for job in self.inner.catalog.recover_scrape_jobs().await? {
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
                let result = engine.run_plugin(job.root_binary_id, &plugin, input).await;
                match &result {
                    Ok(()) => {
                        engine
                            .inner
                            .catalog
                            .set_scrape_attempt(id, &plugin, ScrapeAttemptState::Completed, None)
                            .await?
                    }
                    Err(error) => {
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
        let mut inputs = std::iter::repeat_with(|| None)
            .take(results.len())
            .collect::<Vec<_>>();
        let mut media = Vec::new();
        for (index, result) in results.into_iter().enumerate() {
            if result.key.starts_with("media.") {
                let MetadataValue::String(url) = result.value else {
                    return Err(ScrapingError::InvalidMedia(
                        "media value is not a URL string".to_owned(),
                    ));
                };
                media.push((index, result.key, result.source, url));
            } else {
                inputs[index] = Some(MetadataInput {
                    key: result.key,
                    source: result.source,
                    data: MetadataInputData::Scalar {
                        value: result.value,
                    },
                });
            }
        }
        let temporary_path = temporary.path().to_owned();
        let attempt_bytes = Arc::new(AtomicU64::new(0));
        let downloaded = futures_util::stream::iter(media)
            .map(|(index, key, source, url)| {
                let directory = temporary_path.clone();
                let attempt_bytes = attempt_bytes.clone();
                async move {
                    let (path, media_type) = self
                        .download(&url, &directory, index, &attempt_bytes)
                        .await?;
                    Ok::<_, ScrapingError>((index, key, source, url, path, media_type))
                }
            })
            .buffer_unordered(MAX_CONCURRENT_MEDIA_DOWNLOADS)
            .try_collect::<Vec<_>>()
            .await?;
        for (index, key, source, url, path, media_type) in downloaded {
            inputs[index] = Some(MetadataInput {
                key,
                source,
                data: MetadataInputData::Object {
                    media_type,
                    path,
                    source_url: Some(url),
                },
            });
        }
        let inputs = inputs
            .into_iter()
            .map(|value| value.expect("every scrape result was staged"))
            .collect();
        self.inner
            .catalog
            .replace_metadata(root, &format!("scraper:{plugin}"), inputs)
            .await?;
        self.publish_library_content(root);
        Ok(())
    }

    async fn download(
        &self,
        url: &str,
        directory: &std::path::Path,
        index: usize,
        attempt_bytes: &AtomicU64,
    ) -> Result<(PathBuf, String), ScrapingError> {
        let response = self
            .inner
            .client
            .get(url)
            .send()
            .await?
            .error_for_status()?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_MEDIA_BYTES)
        {
            return Err(ScrapingError::InvalidMedia(
                "media object exceeds 64 MiB".to_owned(),
            ));
        }
        let media_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .filter(|value| {
                value.starts_with("image/")
                    || value.starts_with("audio/")
                    || value.starts_with("video/")
                    || value.starts_with("model/")
            })
            .ok_or_else(|| {
                ScrapingError::InvalidMedia(
                    "media response has no supported Content-Type".to_owned(),
                )
            })?
            .to_owned();
        let path = directory.join(format!("media-{index}"));
        let mut output = tokio::fs::File::create(&path).await?;
        let mut stream = response.bytes_stream();
        let mut length = 0_u64;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            let chunk_length = u64::try_from(chunk.len())
                .map_err(|_| ScrapingError::InvalidMedia("media chunk size overflow".to_owned()))?;
            length = length.checked_add(chunk_length).ok_or_else(|| {
                ScrapingError::InvalidMedia("media object size overflow".to_owned())
            })?;
            if length > MAX_MEDIA_BYTES {
                return Err(ScrapingError::InvalidMedia(
                    "media object exceeds 64 MiB".to_owned(),
                ));
            }
            attempt_bytes
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |total| {
                    total
                        .checked_add(chunk_length)
                        .filter(|total| *total <= MAX_ATTEMPT_BYTES)
                })
                .map_err(|_| {
                    ScrapingError::InvalidMedia("scraper media attempt exceeds 512 MiB".to_owned())
                })?;
            output.write_all(&chunk).await?;
        }
        output.flush().await?;
        Ok((path, media_type))
    }
}
