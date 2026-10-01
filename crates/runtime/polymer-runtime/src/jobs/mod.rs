mod import;

use actix::{Actor, Addr, Context, Handler, ResponseFuture, System};
use import::Importer;
use polymer_deno::polymer::jobs::*;
use polymer_library::jobs::{JobError, JobKind, JobSpec, JobStore};
use std::{future::Future, sync::Arc};
use uuid::Uuid;

/// Lists and controls jobs from every source, and retries finished ones.
#[derive(Clone)]
pub struct JobService {
    actor: Addr<JobActor>,
}

struct JobActor {
    store: Option<Arc<JobStore>>,
    scraping: Option<polymer_scraping::ScrapingEngine>,
    storage: Option<crate::StorageService>,
    importer: Option<Importer>,
}

impl Actor for JobActor {
    type Context = Context<Self>;

    fn started(&mut self, _: &mut Context<Self>) {
        let Some(importer) = self.importer.clone() else {
            return;
        };
        actix::spawn(async move {
            if let Err(error) = importer.resume().await {
                tracing::warn!(%error, "could not resume import jobs");
            }
        });
    }

    fn stopped(&mut self, _: &mut Context<Self>) {
        System::current().stop();
    }
}

impl JobActor {
    fn execute<F, T>(
        &self,
        operation: impl FnOnce(Arc<JobStore>) -> F + 'static,
    ) -> ResponseFuture<Result<T, JobsError>>
    where
        F: Future<Output = Result<T, JobsError>> + 'static,
        T: 'static,
    {
        let store = self.store.clone();
        Box::pin(async move {
            operation(store.ok_or_else(|| JobsError("jobs are unavailable".to_owned()))?).await
        })
    }

    fn importer(&self) -> Result<Importer, JobsError> {
        self.importer
            .clone()
            .ok_or_else(|| JobsError("imports are unavailable".to_owned()))
    }

    fn parse(id: &str) -> Result<Uuid, JobsError> {
        Uuid::parse_str(id).map_err(|_| JobsError(format!("`{id}` is not a job id")))
    }
}

impl Handler<JobsList> for JobActor {
    type Result = ResponseFuture<Result<Vec<Job>, JobsError>>;

    fn handle(&mut self, _: JobsList, _: &mut Context<Self>) -> Self::Result {
        self.execute(|store| async move { store.list().await.map_err(JobsError::from_display) })
    }
}

impl Handler<JobsGet> for JobActor {
    type Result = ResponseFuture<Result<Option<Job>, JobsError>>;

    fn handle(&mut self, JobsGet { id }: JobsGet, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |store| async move {
            store
                .get(Self::parse(&id)?)
                .await
                .map_err(JobsError::from_display)
        })
    }
}

impl Handler<JobsCancel> for JobActor {
    type Result = ResponseFuture<Result<Job, JobsError>>;

    fn handle(&mut self, JobsCancel { id }: JobsCancel, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |store| async move {
            store
                .cancel(Self::parse(&id)?)
                .await
                .map_err(JobsError::from_display)
        })
    }
}

impl Handler<JobsDismiss> for JobActor {
    type Result = ResponseFuture<Result<(), JobsError>>;

    fn handle(&mut self, JobsDismiss { id }: JobsDismiss, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |store| async move {
            store
                .dismiss(Self::parse(&id)?)
                .await
                .map_err(JobsError::from_display)
        })
    }
}

impl Handler<JobsImport> for JobActor {
    type Result = ResponseFuture<Result<Job, JobsError>>;

    fn handle(
        &mut self,
        JobsImport { path, storage }: JobsImport,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let importer = self.importer();
        self.execute(move |_| async move {
            importer?
                .enqueue(Importer::spec(&path, storage.as_deref()))
                .await
        })
    }
}

impl Handler<JobsRetry> for JobActor {
    type Result = ResponseFuture<Result<Job, JobsError>>;

    fn handle(&mut self, JobsRetry { id }: JobsRetry, _: &mut Context<Self>) -> Self::Result {
        let (scraping, storage) = (self.scraping.clone(), self.storage.clone());
        let importer = self.importer();
        self.execute(move |store| async move {
            let id = Self::parse(&id)?;
            let job = store
                .get(id)
                .await
                .map_err(JobsError::from_display)?
                .ok_or_else(|| JobsError::from_display(JobError::NotFound(id)))?;
            if job.state.active() {
                return Err(JobsError::from_display(JobError::InvalidState(
                    id, job.state,
                )));
            }
            match (job.kind, scraping, storage) {
                (JobKind::Import, _, _) => {
                    let path = job.params["path"].as_str().unwrap_or_default();
                    let storage = job.params["storage"].as_str();
                    importer?.enqueue(Importer::spec(path, storage)).await
                }
                (JobKind::Scrape, Some(scraping), _) => {
                    scraping.retry(id).await.map_err(JobsError::from_display)
                }
                (JobKind::Install | JobKind::Bootstrap, _, Some(storage)) => {
                    let spec = JobSpec {
                        kind: job.kind,
                        title: job.title,
                        subject: job.subject,
                        params: job.params,
                        dedupe: None,
                        steps: Vec::new(),
                        persistent: true,
                    };
                    let (job, _) = store.enqueue(spec).await.map_err(JobsError::from_display)?;
                    storage.run_job(job.id);
                    Ok(job)
                }
                (kind, _, _) => Err(JobsError(format!(
                    "{} jobs cannot be retried",
                    kind.as_str()
                ))),
            }
        })
    }
}

impl JobService {
    pub fn start(
        store: Option<Arc<JobStore>>,
        scraping: Option<polymer_scraping::ScrapingEngine>,
        storage: Option<crate::StorageService>,
        library: Option<polymer_deno::polymer::Library>,
    ) -> Result<Self, std::io::Error> {
        let (ready, startup) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("polymer-jobs".to_owned())
            .spawn(move || {
                let system = System::new();
                let actor = system.block_on(async move {
                    let importer = match (&store, library, &storage) {
                        (Some(store), Some(library), Some(storage)) => {
                            Some(Importer::new(store.clone(), library, storage.clone()))
                        }
                        _ => None,
                    };
                    JobActor {
                        store,
                        scraping,
                        storage,
                        importer,
                    }
                    .start()
                });
                if ready.send(actor).is_ok()
                    && let Err(error) = system.run()
                {
                    tracing::error!(%error, "jobs actor system failed");
                }
            })?;
        let actor = startup.recv().map_err(std::io::Error::other)?;
        Ok(Self { actor })
    }

    pub fn client(&self) -> Jobs {
        Jobs {
            list: self.actor.clone().recipient(),
            get: self.actor.clone().recipient(),
            cancel: self.actor.clone().recipient(),
            retry: self.actor.clone().recipient(),
            dismiss: self.actor.clone().recipient(),
            import: self.actor.clone().recipient(),
        }
    }
}
