use super::StorageActor;
use actix::{Context, Handler, Message, ResponseActFuture};
use polymer_deno::polymer::consolestorage::StorageError;
use polymer_library::{
    StorageKind, StorageManager,
    jobs::{JobKind, JobSpec, JobState},
};
use polymer_pluginhost::runtime::PluginRuntimeHandle;
use serde_json::json;
use uuid::Uuid;

static CREATING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Provides the platform's NAND for an import or update, creating it when there is none.
#[derive(Message)]
#[rtype(result = "Result<Uuid, StorageError>")]
pub(crate) struct ProvideNand {
    pub platform: String,
    pub bootstrap: bool,
    pub parent: Option<Uuid>,
}

impl Handler<ProvideNand> for StorageActor {
    type Result = ResponseActFuture<Self, Result<Uuid, StorageError>>;

    fn handle(&mut self, message: ProvideNand, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |manager, plugins| async move {
            Self::provide_nand(
                &manager,
                &plugins,
                &message.platform,
                message.bootstrap,
                message.parent,
            )
            .await
        })
    }
}

impl super::StorageService {
    pub(crate) async fn provide_nand(
        &self,
        platform: &str,
        bootstrap: bool,
        parent: Option<Uuid>,
    ) -> Result<Uuid, StorageError> {
        self.actor
            .send(ProvideNand {
                platform: platform.to_owned(),
                bootstrap,
                parent,
            })
            .await
            .map_err(StorageError::from_display)?
    }
}

impl StorageActor {
    /// The platform's oldest NAND, which launches use by default. Without one, a NAND is
    /// created and, when `bootstrap` is set and a platform plugin provides it, bootstrapped
    /// by a child job of `parent`. A NAND whose bootstrap is still running is waited on.
    pub(super) async fn provide_nand(
        manager: &StorageManager,
        plugins: &PluginRuntimeHandle,
        platform: &str,
        bootstrap: bool,
        parent: Option<Uuid>,
    ) -> Result<Uuid, StorageError> {
        let jobs = Self::jobs(plugins)?;
        let (id, bootstrapping) = {
            let _creating = CREATING.lock().await;
            match manager
                .oldest(StorageKind::Nand, platform)
                .map_err(StorageError::from_display)?
            {
                Some(id) => (id, None),
                None => {
                    let id = manager
                        .default_volume(StorageKind::Nand, platform)
                        .await
                        .map_err(StorageError::from_display)?;
                    let mut job = None;
                    if bootstrap && plugins.registry().platform(platform).is_some() {
                        let spec = JobSpec {
                            parent,
                            ..super::StorageService::bootstrap_spec(platform, id, json!({}))
                        };
                        job = Some(
                            jobs.submit(spec)
                                .await
                                .map_err(StorageError::from_display)?,
                        );
                    }
                    (id, job)
                }
            }
        };
        let bootstrapping = match bootstrapping {
            Some(job) => Some(job),
            None => jobs
                .latest(JobKind::Bootstrap, &id.simple().to_string())
                .await
                .map_err(StorageError::from_display)?
                .filter(|job| job.state.active()),
        };
        if let Some(job) = bootstrapping {
            let done = jobs
                .wait(job.id)
                .await
                .map_err(StorageError::from_display)?;
            if done.state != JobState::Completed {
                return Err(StorageError(done.error.unwrap_or_else(|| {
                    format!("{} was {}", done.title, done.state.as_str())
                })));
            }
        }
        Ok(id)
    }
}
