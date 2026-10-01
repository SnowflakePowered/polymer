use super::StorageActor;
use actix::{Context, Handler, ResponseActFuture};
use polymer_deno::polymer::consolestorage::*;
use polymer_library::StorageKind;
use polymer_library::jobs::JobKind;
use polymer_vfs::ExclusiveResourceKey;
use serde_json::json;
use std::sync::Arc;

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
            let id = Self::parse(&id)?;
            let key = ExclusiveResourceKey::Resource(id);
            let busy = key.is_held();
            let _lease = (!busy)
                .then(|| key.acquire())
                .transpose()
                .map_err(StorageError::from_display)?;
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
                .map_err(StorageError::from_display)?;
            manager
                .open(StorageKind::Nand, &platform, id)
                .await
                .map_err(StorageError::from_display)?
                .as_nand()
                .ok_or("device is not a NAND")?
                .recover()
                .map_err(StorageError::from_display)?;
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
        self.execute(move |manager, plugins| async move {
            let id = Self::parse(&id)?;
            let filename = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            let spec = Self::job_spec(
                JobKind::Install,
                filename,
                json!({ "platform": platform, "storage": id.simple().to_string(), "path": path }),
            );
            let job = Self::jobs(&plugins)?
                .start(spec)
                .await
                .map_err(StorageError::from_display)?;
            Self::install_job(manager, Arc::new(job), false).await
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
            let spec = Self::job_spec(
                JobKind::Bootstrap,
                "Installing system files".to_owned(),
                json!({ "platform": platform, "storage": id.simple().to_string(), "settings": settings }),
            );
            let job = Self::jobs(&plugins)?
                .start(spec)
                .await
                .map_err(StorageError::from_display)?;
            Self::bootstrap_job(manager, plugins, Arc::new(job), false).await
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
                .map_err(StorageError::from_display)?;
            let nand = manager
                .open(StorageKind::Nand, &platform, id)
                .await
                .map_err(StorageError::from_display)?
                .as_nand()
                .ok_or("device is not a NAND")?;
            if nand.is_dirty().map_err(StorageError::from_display)? {
                return Err(
                    "Recover the NAND's uncommitted changes before removing artifacts.".into(),
                );
            }
            if nand
                .root()
                .artifacts()
                .revision()
                .map_err(StorageError::from_display)?
                != revision
            {
                return Err("NAND contents changed. Review the removal again.".into());
            }
            nand.uninstall(&ids.iter().map(String::as_str).collect::<Vec<_>>())
                .await
                .map_err(StorageError::from_display)?;
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
                .map_err(StorageError::from_display)?;
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
                .map_err(StorageError::from_display)?;
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
