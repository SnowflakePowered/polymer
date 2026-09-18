use super::StorageActor;
use actix::{Context, Handler, ResponseActFuture};
use polymer_deno::polymer::consolestorage::*;
use polymer_pluginhost::{
    platform::{DepotSource, Device},
    runtime::EventEmitter,
};
use polymer_vfs::ExclusiveResourceKey;
use serde_json::json;
use std::{path::PathBuf, sync::Arc};

impl Handler<Platforms> for StorageActor {
    type Result = ResponseActFuture<Self, Result<Vec<StoragePlatform>, StorageError>>;

    fn handle(&mut self, _: Platforms, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |_, plugins| async move {
            Ok(plugins
                .registry()
                .plugins::<dyn polymer_pluginhost::platform::PlatformPluginInstance>()
                .iter()
                .flat_map(|plugin| {
                    plugin.platforms().iter().map(|platform| StoragePlatform {
                        platform: platform.to_string(),
                        plugin: plugin.id().to_string(),
                    })
                })
                .collect::<Vec<_>>())
        })
    }
}

impl Handler<Inspect> for StorageActor {
    type Result = ResponseActFuture<Self, Result<StorageInspection, StorageError>>;

    fn handle(&mut self, Inspect { platform, id }: Inspect, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |manager, _| async move {
            let device = Self::device(platform, id)?;
            let _lease = ExclusiveResourceKey::Resource(device.id)
                .acquire()
                .map_err(|error| error.to_string())?;
            device.inspect(&manager).await.map_err(StorageError::from)
        })
    }
}

impl Handler<Recover> for StorageActor {
    type Result = ResponseActFuture<Self, Result<StorageInspection, StorageError>>;

    fn handle(&mut self, Recover { platform, id }: Recover, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |manager, _| async move {
            let device = Self::device(platform, id)?;
            let _lease = ExclusiveResourceKey::Resource(device.id)
                .acquire()
                .map_err(|error| error.to_string())?;
            device
                .open(&manager)
                .await?
                .recover()
                .map_err(|error| error.to_string())?;
            device.inspect(&manager).await.map_err(StorageError::from)
        })
    }
}

impl Handler<Install> for StorageActor {
    type Result = ResponseActFuture<Self, Result<StorageArtifact, StorageError>>;

    fn handle(
        &mut self,
        Install {
            platform,
            id,
            source,
        }: Install,
        _: &mut Context<Self>,
    ) -> Self::Result {
        self.execute(move |manager, _| async move {
            let device = Self::device(platform, id)?;
            let source: DepotSource =
                serde_json::from_value(source).map_err(|error| error.to_string())?;
            let _lease = ExclusiveResourceKey::Resource(device.id)
                .acquire()
                .map_err(|error| error.to_string())?;
            if source.cache.is_some() {
                return Err("Frontend installations require a local source".into());
            }
            let path = PathBuf::from(&source.path);
            let nand = device.open(&manager).await?;
            let key_root = manager.root().join("keys");
            let installation = nand.try_install(&path).map_err(|error| error.to_string())?;
            let records = installation.embedded_keys();
            if !records.is_empty() {
                use polymer_consolestorage::keys::KeyWriter;
                let store = polymer_library::keys::KeyStore::open(key_root)
                    .await
                    .map_err(|error| error.to_string())?;
                store
                    .for_platform(&device.platform)
                    .map_err(|error| error.to_string())?
                    .put(records)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            let events = Arc::new(EventEmitter::new(&device.platform));
            let progress = events.scope(
                "loadprogress",
                json!({"state":"loading","message":"Installing contents"}),
                json!({"state":"failed","message":"Installation interrupted"}),
            );
            let result = DepotSource::finish_install(installation, |message| {
                events.publish("loadprogress", json!({"state":"loading","message":message}))
            })
            .await;
            progress.finish(match &result {
                Ok(_) => json!({"state":"ready","message":"Installation complete"}),
                Err(error) => json!({"state":"failed","message":error}),
            });
            result.map_err(StorageError::from)
        })
    }
}

impl Handler<Bootstrap> for StorageActor {
    type Result = ResponseActFuture<Self, Result<StorageInspection, StorageError>>;

    fn handle(
        &mut self,
        Bootstrap {
            platform,
            id,
            settings,
        }: Bootstrap,
        _: &mut Context<Self>,
    ) -> Self::Result {
        self.execute(move |_, plugins| async move {
            let device = Self::device(platform, id)?;
            let _lease = ExclusiveResourceKey::Resource(device.id)
                .acquire()
                .map_err(|error| error.to_string())?;
            plugins
                .bootstrap(device, settings, plugins.cancellation_token())
                .await
                .map_err(StorageError::from_display)
        })
    }
}

impl Handler<Uninstall> for StorageActor {
    type Result = ResponseActFuture<Self, Result<Vec<StorageArtifact>, StorageError>>;

    fn handle(
        &mut self,
        Uninstall {
            platform,
            id,
            ids,
            revision,
        }: Uninstall,
        _: &mut Context<Self>,
    ) -> Self::Result {
        self.execute(move |manager, _| async move {
            let device = Self::device(platform, id)?;
            let _lease = ExclusiveResourceKey::Resource(device.id)
                .acquire()
                .map_err(|error| error.to_string())?;
            let nand = device.open(&manager).await?;
            if nand.is_dirty().map_err(|error| error.to_string())? {
                return Err(
                    "Recover the NAND's uncommitted changes before removing artifacts.".into(),
                );
            }
            if nand
                .root()
                .artifacts()
                .revision()
                .map_err(|error| error.to_string())?
                != revision
            {
                return Err("NAND contents changed. Review the removal again.".into());
            }
            nand.uninstall(&ids.iter().map(String::as_str).collect::<Vec<_>>())
                .await
                .map_err(|error| error.to_string())?;
            nand.artifacts().await.map_err(StorageError::from_display)
        })
    }
}

impl Handler<PlanRemoval> for StorageActor {
    type Result = ResponseActFuture<Self, Result<StorageRemovalPlan, StorageError>>;

    fn handle(
        &mut self,
        PlanRemoval { platform, id, ids }: PlanRemoval,
        _: &mut Context<Self>,
    ) -> Self::Result {
        self.execute(move |manager, _| async move {
            let device = Self::device(platform, id)?;
            let _lease = ExclusiveResourceKey::Resource(device.id)
                .acquire()
                .map_err(|error| error.to_string())?;
            let nand = device.open(&manager).await?;
            let affected = nand
                .root()
                .artifacts()
                .affected(&ids.iter().map(String::as_str).collect::<Vec<_>>())
                .await
                .map_err(|error| error.to_string())?;
            Ok(StorageRemovalPlan {
                requested: ids,
                affected,
                revision: nand
                    .root()
                    .artifacts()
                    .revision()
                    .map_err(StorageError::from_display)?,
            })
        })
    }
}

impl StorageActor {
    fn device(platform: String, id: String) -> Result<Device, StorageError> {
        Ok(Device {
            platform,
            id: Self::parse(&id)?,
        })
    }
}
