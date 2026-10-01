use polymer_deno::polymer::{
    ConsoleStorage, Library,
    consolestorage::{Create, List, Platforms},
    jobs::{Job, JobsError},
    library::{InstalledArtifact, LaunchablesAdd, LibraryImport},
};
use polymer_library::jobs::{JobHandle, JobKind, JobProgress, JobSpec, JobState, JobStore};
use serde_json::{Value, json};
use std::{future::Future, path::Path, sync::Arc};
use uuid::Uuid;

/// Runs import jobs: identifies a dropped path, then installs and registers what it
/// contains. Installs and bootstraps run as child jobs whose ids are recorded before
/// they start, so a resumed import waits on them instead of repeating them.
#[derive(Clone)]
pub(super) struct Importer {
    store: Arc<JobStore>,
    library: Library,
    storage: crate::StorageService,
    platforms: Arc<dashmap::DashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl Importer {
    pub(super) fn new(
        store: Arc<JobStore>,
        library: Library,
        storage: crate::StorageService,
    ) -> Self {
        Self {
            store,
            library,
            storage,
            platforms: Default::default(),
        }
    }

    pub(super) fn spec(path: &str, storage: Option<&str>) -> JobSpec {
        let title = Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_owned());
        JobSpec {
            kind: JobKind::Import,
            title,
            subject: Default::default(),
            params: json!({ "path": path, "storage": storage }),
            dedupe: Some(path.to_owned()),
            steps: vec!["identify".to_owned()],
            persistent: true,
        }
    }

    /// Queues an import and starts it, unless the same path is already importing.
    pub(super) async fn enqueue(&self, spec: JobSpec) -> Result<Job, JobsError> {
        let (job, created) = self
            .store
            .enqueue(spec)
            .await
            .map_err(JobsError::from_display)?;
        if created {
            self.spawn(job.id);
        }
        Ok(job)
    }

    pub(super) async fn resume(&self) -> Result<(), JobsError> {
        let jobs = self
            .store
            .recover(JobKind::Import)
            .await
            .map_err(JobsError::from_display)?;
        for job in jobs {
            self.spawn(job.id);
        }
        Ok(())
    }

    fn spawn(&self, id: Uuid) {
        let importer = self.clone();
        actix::spawn(async move {
            if let Err(error) = importer.run(id).await {
                tracing::warn!(%error, job = %id, "import job failed");
            }
        });
    }

    async fn run(&self, id: Uuid) -> Result<(), JobsError> {
        let Some(job) = self
            .store
            .claim(id)
            .await
            .map_err(JobsError::from_display)?
        else {
            return Ok(());
        };
        let outcome = self.execute(&job).await;
        let (state, error) = match &outcome {
            Ok(()) => (JobState::Completed, None),
            Err(error) => (JobState::Failed, Some(error.to_string())),
        };
        job.finish(state, error)
            .await
            .map_err(JobsError::from_display)?;
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
            Self::call(&self.library.launchables_add, message)
                .await
                .map(drop)
        })
        .await
    }

    /// The NAND to install into: the job's `storage` parameter when set, otherwise the
    /// platform's first NAND, created and optionally bootstrapped when there is none.
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
        let lock = self
            .platforms
            .entry(platform.to_owned())
            .or_default()
            .clone();
        let _setup = lock.lock().await;
        let bootstrapping = format!("bootstrap:{platform}");
        if let Some(child) = Self::child(&current, &bootstrapping) {
            self.wait(child).await?;
        }
        let volumes = Self::call(&client.list, List).await?;
        let id = match volumes
            .iter()
            .find(|volume| volume.kind == "nand" && volume.platform == platform)
        {
            Some(volume) => Self::parse(&volume.id)?,
            None => self.create(job, &client, platform, bootstrap).await?,
        };
        let mut params = current.params;
        params["storage"] = json!(id.simple().to_string());
        job.reparameterize(params)
            .await
            .map_err(JobsError::from_display)?;
        Ok(id)
    }

    async fn create(
        &self,
        job: &JobHandle,
        client: &ConsoleStorage,
        platform: &str,
        bootstrap: bool,
    ) -> Result<Uuid, JobsError> {
        let name = consolespec::MachineSpec::try_from(platform)
            .map(|machine| machine.name())
            .unwrap_or(platform);
        let create = Create {
            kind: "nand".to_owned(),
            platform: platform.to_owned(),
            label: Some(format!("{name} NAND")),
        };
        let id = Self::parse(&Self::call(&client.create, create).await?.id)?;
        let provided = Self::call(&client.platforms, Platforms)
            .await?
            .iter()
            .any(|provider| provider.platform == platform);
        if bootstrap && provided {
            let spec = crate::StorageService::bootstrap_spec(platform, id, json!({}));
            let child = self
                .start(job, &format!("bootstrap:{platform}"), spec)
                .await?;
            self.wait(child).await?;
        }
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
        self.storage.run_job(child.id);
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
