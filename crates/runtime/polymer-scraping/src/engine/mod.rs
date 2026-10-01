mod media;

use polymer_library::{
    GameCatalog, GameCatalogError, MetadataData, MetadataValue,
    jobs::{
        Job, JobError, JobHandle, JobKind, JobOutcome, JobProgress, JobRunner, JobSpec, JobState,
        JobStore, JobSubject,
    },
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
    LibraryContentChanged { root_binary_id: Uuid },
}

struct EngineInner {
    catalog: Arc<GameCatalog>,
    jobs: Arc<JobStore>,
    _runtime: Arc<PluginRuntime>,
    handle: PluginRuntimeHandle,
    client: reqwest::Client,
    events: tokio::sync::broadcast::Sender<ScrapingEvent>,
}

#[derive(Debug, thiserror::Error)]
pub enum ScrapingError {
    #[error("could not identify {path}: {source}")]
    Detection {
        path: PathBuf,
        #[source]
        source: gumshoe::error::IdentificationError,
    },
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
    Job(#[from] JobError),
    #[error("the game was removed from the library")]
    GameRemoved,
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

impl ScrapingError {
    pub fn firmware_candidate(&self) -> bool {
        matches!(
            self,
            Self::Detection {
                source: gumshoe::error::IdentificationError::Unknown
                    | gumshoe::error::IdentificationError::InvalidData { .. },
                ..
            }
        )
    }
}

impl JobRunner for ScrapingEngine {
    fn run(&self, _: &Arc<JobStore>, ids: Vec<Uuid>, _: bool) {
        for id in ids {
            let engine = self.clone();
            tokio::spawn(async move {
                if let Err(error) = engine.run_job(id).await {
                    tracing::warn!(%error, job = %id, "scrape job failed");
                }
            });
        }
    }
}

impl ScrapingEngine {
    pub fn new(
        catalog: Arc<GameCatalog>,
        jobs: Arc<JobStore>,
        registry: PluginRegistry,
        directories: polymer_library::Directories,
    ) -> Self {
        let runtime = Arc::new(PluginRuntime::new(
            registry,
            directories,
            PluginRuntimeOptions {
                max_concurrent_calls: 8,
                jobs: Some(jobs.clone()),
            },
        ));
        Self::with_runtime(catalog, jobs, runtime)
    }

    fn with_runtime(
        catalog: Arc<GameCatalog>,
        jobs: Arc<JobStore>,
        runtime: Arc<PluginRuntime>,
    ) -> Self {
        let handle = runtime.handle();
        let (events, _) = tokio::sync::broadcast::channel(64);
        let engine = Self {
            inner: Arc::new(EngineInner {
                catalog,
                jobs,
                _runtime: runtime,
                handle,
                client: reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::limited(10))
                    .timeout(MEDIA_REQUEST_TIMEOUT)
                    .build()
                    .expect("scraper HTTP client"),
                events,
            }),
        };
        engine
            .inner
            .jobs
            .register(&[JobKind::Scrape, JobKind::Refresh], Arc::new(engine.clone()));
        engine
    }

    pub fn catalog(&self) -> &Arc<GameCatalog> {
        &self.inner.catalog
    }

    pub fn jobs(&self) -> &Arc<JobStore> {
        &self.inner.jobs
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

    pub async fn enqueue(&self, root: Uuid, trigger: Uuid) -> Result<Job, ScrapingError> {
        if self.inner.catalog.root_binary(trigger).await? != root {
            return Err(GameCatalogError::InvalidData(
                "scrape trigger does not belong to the root game".to_owned(),
            )
            .into());
        }
        let input = self.input(trigger).await?;
        let plugins = self
            .scrapers()
            .iter()
            .filter(|plugin| plugin.accepts(&input))
            .map(|plugin| plugin.id().to_owned())
            .collect::<Vec<_>>();
        let (root, trigger) = (root.simple().to_string(), trigger.simple().to_string());
        let job = Box::pin(self.inner.jobs.submit(JobSpec {
            kind: JobKind::Scrape,
            parent: None,
            title: "Scraping metadata".to_owned(),
            subject: JobSubject {
                binary_id: Some(root.clone()),
                ..Default::default()
            },
            params: serde_json::json!({ "rootBinaryId": root, "triggerBinaryId": trigger }),
            dedupe: Some(format!("{root}:{trigger}")),
            steps: plugins,
            persistent: true,
        }))
        .await?;
        Ok(job)
    }

    /// Load every scraper and resume jobs interrupted by the previous process.
    pub async fn resume(&self) -> Result<(), ScrapingError> {
        self.inner.handle.load::<dyn ScraperPluginInstance>();
        self.inner
            .jobs
            .resume(&[JobKind::Scrape, JobKind::Refresh])
            .await?;
        Ok(())
    }

    fn targets(job: &Job) -> Result<(Uuid, Uuid), ScrapingError> {
        let binary = |key: &str| {
            job.params
                .get(key)
                .and_then(serde_json::Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
                .ok_or_else(|| JobError::InvalidData(format!("scrape job has no {key}")))
        };
        Ok((binary("rootBinaryId")?, binary("triggerBinaryId")?))
    }

    /// Starts an in-memory `refresh` job that runs the scraper's `updateContents`, or
    /// returns the one already refreshing it.
    pub async fn refresh(&self, plugin_id: &str) -> Result<Job, ScrapingError> {
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
        let spec = JobSpec {
            kind: JobKind::Refresh,
            parent: None,
            title: format!("Updating {plugin_id}"),
            subject: JobSubject {
                plugin_id: Some(plugin_id.to_owned()),
                ..Default::default()
            },
            params: serde_json::Value::Null,
            dedupe: Some(plugin_id.to_owned()),
            steps: Vec::new(),
            persistent: false,
        };
        Ok(self.inner.jobs.submit(spec).await?)
    }

    async fn run_job(&self, id: Uuid) -> Result<(), ScrapingError> {
        let Some(handle) = self.inner.jobs.claim(id).await? else {
            return Ok(());
        };
        let handle = Arc::new(handle);
        let job = handle.job().await?;
        let outcome = match (job.kind, job.subject.plugin_id) {
            (JobKind::Refresh, Some(plugin)) => {
                let cancellation = self.inner.handle.cancellation_token();
                self.inner
                    .handle
                    .update_contents(&plugin, Some(handle.clone()), cancellation)
                    .await
                    .map(|()| JobState::Completed)
                    .map_err(ScrapingError::from)
            }
            _ => self.execute_job(&handle).await,
        };
        if let Err(error) = &outcome {
            tracing::warn!(%error, job = %id, "scraping job failed");
        }
        let settled = outcome.as_ref().map(|state| JobOutcome {
            state: *state,
            progress: None,
        });
        handle.settle(settled).await?;
        outcome.map(|_| ())
    }

    /// Runs every eligible scraper in rounds, holding back a scraper while another eligible
    /// scraper provides one of its inputs. A scraper that records results leaves the job;
    /// one that fails or matches nothing runs again once another scraper's results change
    /// the input. The job ends when no eligible scraper is left to run.
    async fn execute_job(&self, handle: &Arc<JobHandle>) -> Result<JobState, ScrapingError> {
        let job = handle.job().await?;
        let (root, trigger) = Self::targets(&job)?;
        if self
            .inner
            .catalog
            .launchables_for_binary(trigger)
            .await?
            .is_empty()
        {
            return Err(ScrapingError::GameRemoved);
        }
        let mut pending = self
            .scrapers()
            .iter()
            .map(|plugin| plugin.id().to_owned())
            .filter(|plugin| {
                !job.steps
                    .iter()
                    .any(|step| step.key == *plugin && step.state == JobState::Completed)
            })
            .collect::<std::collections::BTreeSet<_>>();
        let mut stale = std::collections::BTreeSet::new();
        loop {
            let input = self.input(trigger).await?;
            let eligible = self
                .scrapers()
                .into_iter()
                .filter(|plugin| {
                    pending.contains(plugin.id())
                        && !stale.contains(plugin.id())
                        && plugin.accepts(&input)
                })
                .collect::<Vec<_>>();
            if eligible.is_empty() {
                break;
            }
            let mut tasks = tokio::task::JoinSet::new();
            for plugin in Self::ready(&eligible, &input) {
                stale.insert(plugin.id().to_owned());
                let engine = self.clone();
                let input = input.clone();
                let plugin = plugin.id().to_owned();
                let handle = handle.clone();
                tasks.spawn(async move {
                    let matched = engine
                        .attempt(&handle, root, trigger, &plugin, input)
                        .await?;
                    Ok::<_, ScrapingError>((plugin, matched))
                });
            }
            let mut progressed = false;
            while let Some(result) = tasks.join_next().await {
                if let Ok(Ok((plugin, true))) = result {
                    pending.remove(&plugin);
                    progressed = true;
                }
            }
            if progressed {
                stale.clear();
            }
        }
        let steps = handle.job().await?.steps;
        let completed = steps
            .iter()
            .filter(|step| step.state == JobState::Completed)
            .count();
        Ok(match (completed, steps.len() - completed) {
            (_, 0) => JobState::Completed,
            (0, _) => JobState::Failed,
            _ => JobState::Partial,
        })
    }

    /// Scrapers that await no input another eligible scraper provides, or every eligible
    /// scraper when they await each other.
    fn ready(
        eligible: &[Arc<dyn ScraperPluginInstance>],
        input: &ScrapeInput,
    ) -> Vec<Arc<dyn ScraperPluginInstance>> {
        let ready = eligible
            .iter()
            .filter(|plugin| {
                !eligible.iter().any(|other| {
                    other.id() != plugin.id()
                        && other.provides().iter().any(|key| plugin.awaits(input, key))
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        if ready.is_empty() {
            eligible.to_vec()
        } else {
            ready
        }
    }

    /// Run one scraper attempt, returning whether it recorded any results.
    async fn attempt(
        &self,
        handle: &JobHandle,
        root: Uuid,
        trigger: Uuid,
        plugin: &str,
        input: ScrapeInput,
    ) -> Result<bool, ScrapingError> {
        let id = handle.id();
        handle.step(plugin, JobState::Running, None).await?;
        tracing::info!(job = %id, scraper = %plugin, "scraper started");
        let (state, error) = match self.run_plugin(root, trigger, plugin, input).await {
            Ok(0) => (JobState::Failed, Some("No matching metadata".to_owned())),
            Ok(_) => (JobState::Completed, None),
            Err(error) => (JobState::Failed, Some(error.to_string())),
        };
        tracing::info!(job = %id, scraper = %plugin, ?state, error = ?error, "scraper finished");
        let job = handle.step(plugin, state, error.as_deref()).await?;
        let finished = job
            .steps
            .iter()
            .filter(|step| matches!(step.state, JobState::Completed | JobState::Failed))
            .count();
        handle
            .progress(JobProgress {
                completed: Some(finished as u64),
                total: Some(job.steps.len() as u64),
                ..Default::default()
            })
            .await?;
        Ok(state == JobState::Completed)
    }

    async fn input(&self, binary: Uuid) -> Result<ScrapeInput, ScrapingError> {
        let mut input = BTreeMap::<String, Vec<ScrapeCandidate>>::new();
        let metadata = self.inner.catalog.metadata(binary).await?;
        for value in metadata {
            // Old catalogs may still contain eagerly imported hash metadata.
            if value.key.starts_with("package.hash.") || value.key == "package.size" {
                continue;
            }
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
        let launchables = self.inner.catalog.launchables_for_binary(binary).await?;
        let packages = tokio::task::spawn_blocking(move || {
            launchables
                .into_iter()
                .map(|launchable| crate::Package::from_launchable(launchable).scrape_value())
                .collect::<Vec<_>>()
        })
        .await
        .map_err(|error| ScrapingError::Io(std::io::Error::other(error)))?;
        Ok(ScrapeInput {
            metadata: input,
            packages,
        })
    }

    async fn run_plugin(
        &self,
        root: Uuid,
        binary: Uuid,
        plugin: &str,
        input: ScrapeInput,
    ) -> Result<usize, ScrapingError> {
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
        if !self.inner.handle.contents_ready(plugin).await? {
            let refresh = self.refresh(plugin).await?;
            let refreshed = self.inner.jobs.wait(refresh.id).await?;
            if refreshed.state != JobState::Completed {
                return Err(PluginInvocationError::new(
                    "Error",
                    "CONTENTS_UNAVAILABLE",
                    refreshed
                        .error
                        .unwrap_or_else(|| format!("{plugin} contents could not be updated")),
                )
                .into());
            }
        }
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
        let recorded = inputs.len();
        self.inner
            .catalog
            .replace_metadata(binary, &format!("scraper:{plugin}"), inputs)
            .await?;
        self.publish_library_content(root);
        Ok(recorded)
    }
}
