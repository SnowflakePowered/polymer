use polymer_deno::polymer::{
    Library,
    consolestorage::{List, Platforms},
    jobs::{Job, JobsError},
    library::{InstalledArtifact, LaunchablesAdd, LibraryImport},
};
use polymer_library::GameCatalog;
use polymer_library::jobs::{
    JobHandle, JobKind, JobOutcome, JobProgress, JobRunner, JobSpec, JobState, JobStore,
};
use serde_json::{Value, json};
use std::{future::Future, path::Path, sync::Arc};
use uuid::Uuid;

/// Runs import jobs: identifies a dropped path, then installs and registers what it
/// contains. Installs run as child jobs whose ids are recorded before they start, so a
/// resumed import waits on them instead of repeating them. A completed import queues an
/// update check for each title it added.
#[derive(Clone)]
pub(super) struct Importer {
    store: Arc<JobStore>,
    library: Library,
    storage: crate::StorageService,
    catalog: Arc<GameCatalog>,
    arbiter: actix::ArbiterHandle,
}

impl JobRunner for Importer {
    fn run(&self, _: &Arc<JobStore>, ids: Vec<Uuid>, _: bool) {
        for id in ids {
            let importer = self.clone();
            self.arbiter.spawn(async move {
                if let Err(error) = importer.run_job(id).await {
                    tracing::warn!(%error, job = %id, "import job failed");
                }
            });
        }
    }
}

impl Importer {
    pub(super) fn new(
        store: Arc<JobStore>,
        library: Library,
        storage: crate::StorageService,
        catalog: Arc<GameCatalog>,
    ) -> Self {
        Self {
            store,
            library,
            storage,
            catalog,
            arbiter: actix::Arbiter::current(),
        }
    }

    pub(super) fn spec(path: &str, storage: Option<&str>) -> JobSpec {
        let title = Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_owned());
        JobSpec {
            kind: JobKind::Import,
            parent: None,
            title,
            subject: Default::default(),
            params: json!({ "path": path, "storage": storage }),
            dedupe: Some(path.to_owned()),
            steps: vec!["identify".to_owned()],
            persistent: true,
        }
    }

    async fn run_job(&self, id: Uuid) -> Result<(), JobsError> {
        let Some(job) = self
            .store
            .claim(id)
            .await
            .map_err(JobsError::from_display)?
        else {
            return Ok(());
        };
        let outcome = self.execute(&job).await;
        job.settle(outcome.as_ref().map(|()| JobOutcome::completed(None)))
            .await
            .map_err(JobsError::from_display)?;
        if outcome.is_ok()
            && let Err(error) = self.check_updates(&job).await
        {
            tracing::warn!(%error, job = %id, "could not check imported titles for updates");
        }
        outcome
    }

    async fn execute(&self, job: &JobHandle) -> Result<(), JobsError> {
        let result = self.identify(job).await?;
        match result["kind"].as_str() {
            Some("system") => self.install(job, &result, false).await.map(drop),
            Some("installables") => {
                for item in result["installables"].as_array().into_iter().flatten() {
                    let installed = self.install(job, item, true).await?;
                    self.add(job, item, &installed).await?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Identifies the path and applies what needs no NAND: keys, firmware, and games.
    async fn identify(&self, job: &JobHandle) -> Result<Value, JobsError> {
        let current = Self::get(job).await?;
        if Self::done(&current, "identify") {
            return Ok(current.detail["result"].clone());
        }
        let path = current.params["path"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        Self::stage(
            job,
            "identify",
            format!("Identifying {}", current.title),
            async {
                let result = Self::call(&self.library.import, LibraryImport { path }).await?;
                Self::note(job, |detail| detail["result"] = result.clone()).await?;
                Ok(result)
            },
        )
        .await
    }

    /// Installs one package into a NAND, returning `{ storage, artifact }`.
    async fn install(
        &self,
        job: &JobHandle,
        item: &Value,
        bootstrap: bool,
    ) -> Result<Value, JobsError> {
        let filename = item["filename"].as_str().unwrap_or_default();
        let key = format!("install:{filename}");
        let current = Self::get(job).await?;
        if Self::done(&current, &key) {
            return Ok(current.detail["installed"][&key].clone());
        }
        Self::stage(job, &key, format!("Installing {filename}"), async {
            let platform = item["platform"]
                .as_str()
                .ok_or_else(|| JobsError("the package has no platform".to_owned()))?;
            let child = match Self::child(&current, &key) {
                Some(child) => child,
                None => {
                    let storage = self.nand(job, platform, bootstrap).await?;
                    let path = Path::new(item["path"].as_str().unwrap_or_default());
                    let spec = crate::StorageService::install_spec(platform, storage, path);
                    self.start(job, &key, spec).await?
                }
            };
            let child = self.wait(child).await?;
            let installed = json!({ "storage": child.subject.storage, "artifact": child.detail });
            Self::note(job, |detail| detail["installed"][&key] = installed.clone()).await?;
            Ok(installed)
        })
        .await
    }

    /// Adds an installed artifact to the library, which queues its scrape.
    async fn add(&self, job: &JobHandle, item: &Value, installed: &Value) -> Result<(), JobsError> {
        let filename = item["filename"].as_str().unwrap_or_default();
        let key = format!("add:{filename}");
        if Self::done(&Self::get(job).await?, &key) {
            return Ok(());
        }
        let text = |value: &Value| value.as_str().map(str::to_owned);
        let artifact = &installed["artifact"];
        let message = LaunchablesAdd {
            path: text(&item["path"]).unwrap_or_default(),
            platform: text(&item["platform"]),
            parent_binary_id: None,
            artifact: Some(InstalledArtifact {
                storage: text(&installed["storage"]).unwrap_or_default(),
                id: text(&artifact["id"]).unwrap_or_default(),
                entrypoint: text(&artifact["entrypoint"]),
                package: text(&artifact["package"]),
            }),
        };
        Self::stage(job, &key, format!("Adding {filename}"), async {
            let added = Self::call(&self.library.launchables_add, message).await?;
            let launchables = std::iter::once(&added["launchable"])
                .chain(added["imports"].as_array().into_iter().flatten().map(|import| &import["launchable"]))
                .map(|launchable| launchable["id"].clone())
                .collect::<Vec<_>>();
            Self::note(job, |detail| detail["added"][&key] = json!(launchables)).await
        })
        .await
    }

    /// Queues a `check` job for each title the import added on a platform whose plugin
    /// provides updates.
    async fn check_updates(&self, job: &JobHandle) -> Result<(), JobsError> {
        let detail = Self::get(job).await?.detail;
        let platforms = Self::call(&self.storage.client().platforms, Platforms).await?;
        let identified = detail["result"]["imports"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|import| &import["launchable"]["id"]);
        let installed = detail["added"]
            .as_object()
            .into_iter()
            .flat_map(|added| added.values())
            .filter_map(Value::as_array)
            .flatten();
        let ids = identified
            .chain(installed)
            .filter_map(Value::as_str)
            .map(Self::parse)
            .collect::<Result<Vec<_>, _>>()?;
        for id in ids {
            let launchable = self
                .catalog
                .launchable(id)
                .await
                .map_err(JobsError::from_display)?;
            let updatable = platforms
                .iter()
                .any(|platform| platform.updates && platform.platform == launchable.identified.platform);
            if !updatable || launchable.identified.title_id.is_none() {
                continue;
            }
            let spec = crate::StorageService::check_spec(&launchable).map_err(JobsError::from_display)?;
            self.store
                .submit(spec)
                .await
                .map_err(JobsError::from_display)?;
        }
        Ok(())
    }

    /// The NAND to install into: the job's `storage` parameter when set, otherwise the
    /// platform's default NAND, created and optionally bootstrapped when there is none.
    /// The choice is written back to the parameters.
    async fn nand(
        &self,
        job: &JobHandle,
        platform: &str,
        bootstrap: bool,
    ) -> Result<Uuid, JobsError> {
        let current = Self::get(job).await?;
        let client = self.storage.client();
        if let Some(storage) = current.params["storage"].as_str() {
            let volumes = Self::call(&client.list, List).await?;
            let volume = volumes
                .iter()
                .find(|volume| volume.kind == "nand" && Self::same(&volume.id, storage))
                .ok_or_else(|| JobsError(format!("NAND `{storage}` does not exist")))?;
            if volume.platform != platform {
                return Err(JobsError(format!(
                    "NAND `{storage}` is for {}, not {platform}",
                    volume.platform
                )));
            }
            return Self::parse(&volume.id);
        }
        let id = self
            .storage
            .provide_nand(platform, bootstrap, Some(job.id()))
            .await
            .map_err(JobsError::from_display)?;
        let mut params = current.params;
        params["storage"] = json!(id.simple().to_string());
        job.reparameterize(params)
            .await
            .map_err(JobsError::from_display)?;
        Ok(id)
    }

    /// Queues a child job, recording its id before it can start.
    async fn start(&self, job: &JobHandle, key: &str, spec: JobSpec) -> Result<Uuid, JobsError> {
        let (child, _) = self
            .store
            .enqueue(spec)
            .await
            .map_err(JobsError::from_display)?;
        Self::note(job, |detail| {
            detail["children"][key] = json!(child.id.simple().to_string())
        })
        .await?;
        self.store
            .dispatch(&child)
            .map_err(JobsError::from_display)?;
        Ok(child.id)
    }

    async fn wait(&self, child: Uuid) -> Result<Job, JobsError> {
        let child = self
            .store
            .wait(child)
            .await
            .map_err(JobsError::from_display)?;
        match child.state {
            JobState::Completed => Ok(child),
            state => Err(JobsError(child.error.unwrap_or_else(|| {
                format!("{} was {}", child.title, state.as_str())
            }))),
        }
    }

    async fn stage<T>(
        job: &JobHandle,
        key: &str,
        message: String,
        work: impl Future<Output = Result<T, JobsError>>,
    ) -> Result<T, JobsError> {
        job.step(key, JobState::Running, None)
            .await
            .map_err(JobsError::from_display)?;
        let progress = JobProgress {
            message: Some(message),
            ..Default::default()
        };
        let _ = job.progress(progress).await;
        let result = work.await;
        let error = result.as_ref().err().map(ToString::to_string);
        let state = if error.is_some() {
            JobState::Failed
        } else {
            JobState::Completed
        };
        job.step(key, state, error.as_deref())
            .await
            .map_err(JobsError::from_display)?;
        result
    }

    async fn note(job: &JobHandle, change: impl FnOnce(&mut Value)) -> Result<(), JobsError> {
        let mut detail = Self::get(job).await?.detail;
        change(&mut detail);
        job.record(detail)
            .await
            .map(drop)
            .map_err(JobsError::from_display)
    }

    async fn get(job: &JobHandle) -> Result<Job, JobsError> {
        job.job().await.map_err(JobsError::from_display)
    }

    async fn call<M, T, E>(recipient: &actix::Recipient<M>, message: M) -> Result<T, JobsError>
    where
        M: actix::Message<Result = Result<T, E>> + Send + 'static,
        T: Send + 'static,
        E: std::fmt::Display + Send + 'static,
    {
        recipient
            .send(message)
            .await
            .map_err(JobsError::from_display)?
            .map_err(JobsError::from_display)
    }

    fn done(job: &Job, key: &str) -> bool {
        job.steps
            .iter()
            .any(|step| step.key == key && step.state == JobState::Completed)
    }

    fn child(job: &Job, key: &str) -> Option<Uuid> {
        job.detail["children"][key]
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
    }

    fn same(left: &str, right: &str) -> bool {
        matches!((Uuid::parse_str(left), Uuid::parse_str(right)), (Ok(left), Ok(right)) if left == right)
    }

    fn parse(id: &str) -> Result<Uuid, JobsError> {
        Uuid::parse_str(id).map_err(|_| JobsError(format!("`{id}` is not a storage id")))
    }
}
