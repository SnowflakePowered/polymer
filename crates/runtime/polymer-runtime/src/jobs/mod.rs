mod import;

use actix::{Actor, Addr, Context, Handler, ResponseFuture, System};
use import::Importer;
use polymer_deno::polymer::jobs::*;
use polymer_library::{
    GameCatalog, Launchable,
    jobs::{JobError, JobKind, JobStore},
};
use std::{future::Future, sync::Arc};
use uuid::Uuid;

/// Lists and controls jobs from every source, and retries finished ones.
#[derive(Clone)]
pub struct JobService {
    actor: Addr<JobActor>,
}

struct JobActor {
    store: Option<Arc<JobStore>>,
    importer: Option<Importer>,
    catalog: Option<Arc<GameCatalog>>,
    storage: Option<crate::StorageService>,
}

impl Actor for JobActor {
    type Context = Context<Self>;

    fn started(&mut self, _: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let importer = self.importer.clone();
        if let Some(importer) = &importer {
            store.register(&[JobKind::Import], Arc::new(importer.clone()));
        }
        if let (Some(catalog), Some(storage)) = (self.catalog.clone(), self.storage.clone()) {
            let store = store.clone();
            actix::spawn(async move {
                let checked = async {
                    let games = catalog
                        .games(None)
                        .await
                        .map_err(|error| error.to_string())?;
                    let launchables = games.into_iter().flat_map(|game| game.launchables);
                    storage
                        .check_titles(&store, launchables)
                        .await
                        .map_err(|error| error.to_string())
                };
                if let Err(error) = checked.await {
                    tracing::warn!(%error, "could not check library titles for updates");
                }
            });
        }
        actix::spawn(async move {
            if let Err(error) = store.abandon(JobKind::Download).await {
                tracing::warn!(%error, "could not fail interrupted downloads");
            }
            if importer.is_some()
                && let Err(error) = store.resume(&[JobKind::Import]).await
            {
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
        F: Future<Output = Result<T, JobError>> + 'static,
        T: 'static,
    {
        let store = self.store.clone();
        Box::pin(async move {
            let store = store.ok_or_else(|| JobsError("jobs are unavailable".to_owned()))?;
            operation(store).await.map_err(JobsError::from_display)
        })
    }

    fn parse(id: &str) -> Result<Uuid, JobError> {
        Uuid::parse_str(id).map_err(|_| JobError::InvalidData(format!("`{id}` is not an id")))
    }

    async fn launchable(
        catalog: Option<Arc<GameCatalog>>,
        id: &str,
    ) -> Result<Launchable, JobError> {
        let catalog = catalog
            .ok_or_else(|| JobError::InvalidData("the library is unavailable".to_owned()))?;
        catalog
            .launchable(Self::parse(id)?)
            .await
            .map_err(|error| JobError::InvalidData(error.to_string()))
    }
}

impl Handler<JobsList> for JobActor {
    type Result = ResponseFuture<Result<Vec<Job>, JobsError>>;

    fn handle(&mut self, _: JobsList, _: &mut Context<Self>) -> Self::Result {
        self.execute(|store| async move { store.list().await })
    }
}

impl Handler<JobsGet> for JobActor {
    type Result = ResponseFuture<Result<Option<Job>, JobsError>>;

    fn handle(&mut self, JobsGet { id }: JobsGet, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |store| async move { store.get(Self::parse(&id)?).await })
    }
}

impl Handler<JobsCancel> for JobActor {
    type Result = ResponseFuture<Result<Job, JobsError>>;

    fn handle(&mut self, JobsCancel { id }: JobsCancel, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |store| async move { store.cancel(Self::parse(&id)?).await })
    }
}

impl Handler<JobsDismiss> for JobActor {
    type Result = ResponseFuture<Result<(), JobsError>>;

    fn handle(&mut self, JobsDismiss { id }: JobsDismiss, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |store| async move { store.dismiss(Self::parse(&id)?).await })
    }
}

impl Handler<JobsImport> for JobActor {
    type Result = ResponseFuture<Result<Job, JobsError>>;

    fn handle(
        &mut self,
        JobsImport { path, storage }: JobsImport,
        _: &mut Context<Self>,
    ) -> Self::Result {
        self.execute(move |store| async move {
            store
                .submit(Importer::spec(&path, storage.as_deref()))
                .await
        })
    }
}

impl Handler<JobsCheckUpdates> for JobActor {
    type Result = ResponseFuture<Result<Job, JobsError>>;

    fn handle(&mut self, message: JobsCheckUpdates, _: &mut Context<Self>) -> Self::Result {
        let catalog = self.catalog.clone();
        self.execute(move |store| async move {
            let launchable = Self::launchable(catalog, &message.launchable).await?;
            let spec = crate::StorageService::check_spec(&launchable)
                .map_err(|error| JobError::InvalidData(error.to_string()))?;
            store.submit(spec).await
        })
    }
}

impl Handler<JobsFetchUpdate> for JobActor {
    type Result = ResponseFuture<Result<Job, JobsError>>;

    fn handle(&mut self, message: JobsFetchUpdate, _: &mut Context<Self>) -> Self::Result {
        let catalog = self.catalog.clone();
        self.execute(move |store| async move {
            let launchable = Self::launchable(catalog, &message.launchable).await?;
            let spec = crate::StorageService::update_spec(&launchable, message.candidate)
                .map_err(|error| JobError::InvalidData(error.to_string()))?;
            store.submit(spec).await
        })
    }
}

impl Handler<JobsRetry> for JobActor {
    type Result = ResponseFuture<Result<Job, JobsError>>;

    fn handle(&mut self, JobsRetry { id }: JobsRetry, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |store| async move { store.retry(Self::parse(&id)?).await })
    }
}

impl JobService {
    pub fn start(
        store: Option<Arc<JobStore>>,
        storage: Option<crate::StorageService>,
        library: Option<polymer_deno::polymer::Library>,
        catalog: Option<Arc<GameCatalog>>,
    ) -> Result<Self, std::io::Error> {
        let (ready, startup) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("polymer-jobs".to_owned())
            .spawn(move || {
                let system = System::new();
                let actor = system.block_on(async move {
                    let importer = match (&store, library, &storage, &catalog) {
                        (Some(store), Some(library), Some(storage), Some(catalog)) => Some(
                            Importer::new(store.clone(), library, storage.clone(), catalog.clone()),
                        ),
                        _ => None,
                    };
                    JobActor {
                        store,
                        importer,
                        catalog,
                        storage,
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
            check_updates: self.actor.clone().recipient(),
            fetch_update: self.actor.clone().recipient(),
        }
    }
}
