mod api;
mod operations;
mod volumes;

use actix::{Actor, Addr, Context, ResponseActFuture, System, WrapFuture};
use dashmap::DashSet;
use polymer_deno::polymer::consolestorage::StorageError;
use polymer_library::{Directories, StorageManager};
use polymer_pluginhost::runtime::PluginRuntimeHandle;
use std::{future::Future, sync::Arc};

#[derive(Clone)]
pub struct StorageService {
    actor: Addr<StorageActor>,
}

struct StorageActor {
    manager: Option<Arc<StorageManager>>,
    plugins: PluginRuntimeHandle,
    installs: Arc<tokio::sync::Mutex<()>>,
    installing: Arc<DashSet<uuid::Uuid>>,
}

impl Actor for StorageActor {
    type Context = Context<Self>;

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
    ) -> Result<Self, std::io::Error> {
        let (ready, startup) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("polymer-storage".to_owned())
            .spawn(move || {
                let system = System::new();
                let actor =
                    system.block_on(async move { StorageActor {
                        manager,
                        plugins,
                        installs: Default::default(),
                        installing: Default::default(),
                    }
                    .start() });
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

pub fn manager() -> Option<Arc<StorageManager>> {
    Directories::discover()
        .ok()
        .map(|directories| Arc::new(StorageManager::new(directories.consolestorage())))
}
