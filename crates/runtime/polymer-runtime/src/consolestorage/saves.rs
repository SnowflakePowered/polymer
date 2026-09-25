use super::StorageActor;
use actix::{Context, Handler, ResponseActFuture};
use base64::Engine as _;
use polymer_consolestorage::savegame::SaveDepotId;
use polymer_consolestorage::{
    Save, SaveAttachmentKind, SaveCommitId, SaveCommitInfo, SaveCommitOperation,
};
use polymer_deno::polymer::consolestorage::*;
use polymer_library::{SaveRepo, StorageKind, StorageManager};
use polymer_vfs::{ExclusiveResourceKey, ResourceLease};
use std::io::Read as _;
use std::sync::Arc;

struct OpenSaves {
    repo: Arc<dyn SaveRepo>,
    _lease: Arc<dyn ResourceLease>,
}

impl OpenSaves {
    async fn open(manager: &StorageManager, volume: &VolumeRef) -> Result<Self, StorageError> {
        let id = StorageActor::parse(&volume.id)?;
        let lease = ExclusiveResourceKey::Resource(id)
            .acquire()
            .map_err(StorageError::from_display)?;
        let kind = volume
            .kind
            .parse::<StorageKind>()
            .map_err(StorageError::from_display)?;
        let repo = manager
            .open(kind, &volume.platform, id)
            .await
            .map_err(StorageError::from_display)?
            .save_repo()
            .cloned()
            .ok_or("device does not hold saves")?;
        Ok(Self {
            repo,
            _lease: lease,
        })
    }

    fn save(&self, save: &str) -> Result<Save<'_>, StorageError> {
        u64::from_str_radix(save, 16)
            .map(|depot| Save::new(&*self.repo, SaveDepotId(depot)))
            .map_err(|_| StorageError(format!("`{save}` is not a save id")))
    }

    fn commit(info: SaveCommitInfo) -> StorageSaveCommit {
        StorageSaveCommit {
            id: info.id.to_string(),
            parent: info.parent.map(|parent| parent.to_string()),
            timestamp: info.timestamp.to_string(),
            operation: match info.operation {
                SaveCommitOperation::InitialImport => "initialImport",
                SaveCommitOperation::Autosave => "autosave",
                SaveCommitOperation::Manual => "manual",
                SaveCommitOperation::Restore { .. } => "restore",
            },
            screenshot: None,
        }
    }

    fn screenshot(save: &Save<'_>, commit: SaveCommitId) -> Option<String> {
        let mut bytes = Vec::new();
        save.commit(commit)
            .ok()?
            .open_attachment(SaveAttachmentKind::Screenshot)
            .ok()??
            .read_to_end(&mut bytes)
            .ok()?;
        let media = if bytes.starts_with(b"\x89PNG") {
            "image/png"
        } else {
            "image/webp"
        };
        Some(format!(
            "data:{media};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    }

    async fn describe(&self) -> Result<Vec<StorageSave>, StorageError> {
        let descriptors = self.repo.enumerate_saves().collect::<Vec<_>>();
        let mut saves = Vec::with_capacity(descriptors.len());
        for descriptor in descriptors {
            let byte_length = self
                .repo
                .export_save(&descriptor.id)
                .map_err(StorageError::from_display)?
                .blobs
                .iter()
                .map(|blob| blob.bytes.len() as u64)
                .sum();
            let icon = match self.repo.save_icon(descriptor.id).await {
                Ok(Some(icon)) => icon.data_url().ok(),
                _ => None,
            };
            saves.push(StorageSave {
                id: format!("{:016x}", descriptor.id.0),
                title: descriptor.title,
                description: descriptor.description,
                modified: descriptor.timestamp.to_string(),
                icon,
                byte_length,
                blocks: self
                    .repo
                    .blocks(descriptor.id)
                    .map_err(StorageError::from_display)?,
                versions: Save::new(&*self.repo, descriptor.id)
                    .history()
                    .map_err(StorageError::from_display)?
                    .len(),
            });
        }
        Ok(saves)
    }
}

impl Handler<ListSaves> for StorageActor {
    type Result = ResponseActFuture<Self, Result<Vec<StorageSave>, StorageError>>;

    fn handle(&mut self, ListSaves { volume }: ListSaves, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |manager, _| async move {
            OpenSaves::open(&manager, &volume).await?.describe().await
        })
    }
}

impl Handler<SaveHistory> for StorageActor {
    type Result = ResponseActFuture<Self, Result<Vec<StorageSaveCommit>, StorageError>>;

    fn handle(
        &mut self,
        SaveHistory { volume, save }: SaveHistory,
        _: &mut Context<Self>,
    ) -> Self::Result {
        self.execute(move |manager, _| async move {
            let saves = OpenSaves::open(&manager, &volume).await?;
            let save = saves.save(&save)?;
            Ok(save
                .history()
                .map_err(StorageError::from_display)?
                .into_iter()
                .map(|info| {
                    let screenshot = OpenSaves::screenshot(&save, info.id);
                    StorageSaveCommit {
                        screenshot,
                        ..OpenSaves::commit(info)
                    }
                })
                .collect())
        })
    }
}

impl Handler<RestoreSave> for StorageActor {
    type Result = ResponseActFuture<Self, Result<StorageSaveCommit, StorageError>>;

    fn handle(
        &mut self,
        RestoreSave {
            volume,
            save,
            commit,
        }: RestoreSave,
        _: &mut Context<Self>,
    ) -> Self::Result {
        self.execute(move |manager, _| async move {
            let saves = OpenSaves::open(&manager, &volume).await?;
            saves
                .save(&save)?
                .restore(commit.parse().map_err(StorageError::from_display)?)
                .await
                .map(OpenSaves::commit)
                .map_err(StorageError::from_display)
        })
    }
}

impl Handler<CopySave> for StorageActor {
    type Result = ResponseActFuture<Self, Result<StorageSaveCommit, StorageError>>;

    fn handle(
        &mut self,
        CopySave {
            volume,
            save,
            destination,
        }: CopySave,
        _: &mut Context<Self>,
    ) -> Self::Result {
        self.execute(move |manager, _| async move {
            if destination.platform != volume.platform {
                return Err("saves can only be copied within one platform".into());
            }
            let source = OpenSaves::open(&manager, &volume).await?;
            let target = OpenSaves::open(&manager, &destination).await?;
            source
                .save(&save)?
                .copy_to(&*target.repo)
                .await
                .map(OpenSaves::commit)
                .map_err(StorageError::from_display)
        })
    }
}

impl Handler<DeleteSave> for StorageActor {
    type Result = ResponseActFuture<Self, Result<(), StorageError>>;

    fn handle(
        &mut self,
        DeleteSave { volume, save }: DeleteSave,
        _: &mut Context<Self>,
    ) -> Self::Result {
        self.execute(move |manager, _| async move {
            let saves = OpenSaves::open(&manager, &volume).await?;
            saves
                .save(&save)?
                .delete()
                .await
                .map_err(StorageError::from_display)
        })
    }
}

impl Handler<Fork> for StorageActor {
    type Result = ResponseActFuture<Self, Result<StorageVolume, StorageError>>;

    fn handle(&mut self, Fork { volume, label }: Fork, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |manager, _| async move {
            let id = Self::parse(&volume.id)?;
            let _lease = ExclusiveResourceKey::Resource(id)
                .acquire()
                .map_err(StorageError::from_display)?;
            let kind = volume
                .kind
                .parse::<StorageKind>()
                .map_err(StorageError::from_display)?;
            let storage = manager
                .fork(kind, &volume.platform, id, label)
                .await
                .map_err(StorageError::from_display)?;
            let metadata = storage
                .metadata()
                .await
                .map_err(StorageError::from_display)?;
            Ok(Self::describe(&manager, kind, &metadata))
        })
    }
}
