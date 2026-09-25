use super::StorageActor;
use actix::{Context, Handler, ResponseActFuture};
use polymer_deno::polymer::consolestorage::*;
use polymer_library::StorageKind;
use polymer_pluginhost::runtime::EventEmitter;
use polymer_vfs::ExclusiveResourceKey;
use serde_json::{Value, json};

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
        let installing = self.installing.clone();
        self.execute(move |manager, _| async move {
            let id = Self::parse(&id)?;
            let busy = installing.contains(&id);
            let _lease = (!busy)
                .then(|| ExclusiveResourceKey::Resource(id).acquire())
                .transpose()
                .map_err(|error| error.to_string())?;
            let mut inspection = manager
                .inspect(&platform, id)
                .await
                .map_err(StorageError::from_display)?;
            inspection.dirty &= !busy;
            Ok(inspection)
        })
    }
}

impl Handler<Recover> for StorageActor {
    type Result = ResponseActFuture<Self, Result<StorageInspection, StorageError>>;

    fn handle(&mut self, Recover { platform, id }: Recover, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |manager, _| async move {
            let id = Self::parse(&id)?;
            let _lease = ExclusiveResourceKey::Resource(id)
                .acquire()
                .map_err(|error| error.to_string())?;
            manager
                .open(StorageKind::Nand, &platform, id)
                .await
                .map_err(StorageError::from_display)?
                .as_nand()
                .ok_or("device is not a NAND")?
                .recover()
                .map_err(|error| error.to_string())?;
            manager
                .inspect(&platform, id)
                .await
                .map_err(StorageError::from_display)
        })
    }
}

impl Handler<Install> for StorageActor {
    type Result = ResponseActFuture<Self, Result<StorageArtifact, StorageError>>;

    fn handle(
        &mut self,
        Install { platform, id, path }: Install,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let installs = self.installs.clone();
        let installing = self.installing.clone();
        self.execute(move |manager, _| async move {
            let id = Self::parse(&id)?;
            let filename = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let source = json!({
                "id": uuid::Uuid::now_v7().simple().to_string(),
                "platform": platform,
                "storage": id.simple().to_string(),
                "filename": filename,
            });
            let events = std::sync::Arc::new(EventEmitter::new(&platform));
            let publish = move |state: &str, detail: Value| {
                let mut event = source.clone();
                event["state"] = json!(state);
                event["detail"] = detail;
                events.publish("installprogress", event);
            };
            let publish = std::sync::Arc::new(publish);
            publish("queued", Value::Null);
            let result = async {
                let _queue = installs.lock().await;
                installing.insert(id);
                let _lease = ExclusiveResourceKey::Resource(id)
                    .acquire()
                    .map_err(|error| error.to_string())?;
                let running = publish.clone();
                manager
                    .install(&platform, id, &path, move |status| running("running", json!(status)))
                    .await
                    .map_err(StorageError::from)
            }
            .await;
            installing.remove(&id);
            match &result {
                Ok(artifact) => publish("complete", json!(artifact)),
                Err(error) => publish("failed", json!(error.to_string())),
            }
            result
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
        self.execute(move |manager, plugins| async move {
            let id = Self::parse(&id)?;
            let _lease = ExclusiveResourceKey::Resource(id)
                .acquire()
                .map_err(|error| error.to_string())?;
            plugins
                .bootstrap(
                    manager,
                    &platform,
                    id,
                    settings,
                    plugins.cancellation_token(),
                )
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
            let id = Self::parse(&id)?;
            let _lease = ExclusiveResourceKey::Resource(id)
                .acquire()
                .map_err(|error| error.to_string())?;
            let nand = manager
                .open(StorageKind::Nand, &platform, id)
                .await
                .map_err(StorageError::from_display)?
                .as_nand()
                .ok_or("device is not a NAND")?;
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
            let id = Self::parse(&id)?;
            let _lease = ExclusiveResourceKey::Resource(id)
                .acquire()
                .map_err(|error| error.to_string())?;
            let nand = manager
                .open(StorageKind::Nand, &platform, id)
                .await
                .map_err(StorageError::from_display)?
                .as_nand()
                .ok_or("device is not a NAND")?;
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
