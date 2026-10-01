mod api;
mod jobs;
mod nand;
mod operations;
mod saves;
mod updates;
mod volumes;

use actix::{Actor, Addr, AsyncContext, Context, ResponseActFuture, System, WrapFuture};
use polymer_deno::polymer::consolestorage::StorageError;
use polymer_library::{GameCatalog, StorageManager, jobs::JobKind};
use polymer_pluginhost::runtime::PluginRuntimeHandle;
use std::{future::Future, sync::Arc};

#[derive(Clone)]
pub struct StorageService {
    actor: Addr<StorageActor>,
}

struct StorageActor {
    manager: Option<Arc<StorageManager>>,
    plugins: PluginRuntimeHandle,
    catalog: Option<Arc<GameCatalog>>,
}

impl Actor for StorageActor {
    type Context = Context<Self>;

    fn started(&mut self, context: &mut Context<Self>) {
        self.plugins
            .load::<dyn polymer_pluginhost::platform::PlatformPluginInstance>();
        let Some(store) = self.plugins.jobs().cloned() else {
            return;
        };
        let runner = Arc::new(StorageService {
            actor: context.address(),
        });
        let kinds = [JobKind::Install, JobKind::Bootstrap, JobKind::Update, JobKind::Check];
        store.register(&kinds, runner);
        actix::spawn(async move {
            if let Err(error) = store.resume(&kinds).await {
                tracing::warn!(%error, "could not resume storage jobs");
            }
        });
    }

    fn stopped(&mut self, _: &mut Context<Self>) {
        System::current().stop();
    }
}

impl StorageActor {
    // Track operations in the actor context so in-flight work keeps the actor
    // alive. Futures may interleave; resource leases protect each device across
    // awaits and also exclude emulators using its mounted filesystem.
    fn execute<F, T>(
        &self,
        operation: impl FnOnce(Arc<StorageManager>, PluginRuntimeHandle) -> F + 'static,
    ) -> ResponseActFuture<Self, Result<T, StorageError>>
    where
        F: Future<Output = Result<T, StorageError>> + 'static,
        T: 'static,
    {
        let manager = self.manager.clone();
        let plugins = self.plugins.clone();
        Box::pin(
            async move {
                let manager = manager
                    .ok_or_else(|| StorageError("console storage is unavailable".to_owned()))?;
                operation(manager, plugins).await
            }
            .into_actor(self),
        )
    }
}

impl StorageService {
    pub fn start(
        manager: Option<Arc<StorageManager>>,
        plugins: PluginRuntimeHandle,
        catalog: Option<Arc<GameCatalog>>,
    ) -> Result<Self, std::io::Error> {
        let (ready, startup) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("polymer-storage".to_owned())
            .spawn(move || {
                let system = System::new();
                let actor = system.block_on(async move {
                    StorageActor {
                        manager,
                        plugins,
                        catalog,
                    }
                    .start()
                });
                if ready.send(actor).is_ok()
                    && let Err(error) = system.run()
                {
                    tracing::error!(%error, "storage actor system failed");
                }
            })?;
        let actor = startup.recv().map_err(std::io::Error::other)?;
        Ok(Self { actor })
    }
}
