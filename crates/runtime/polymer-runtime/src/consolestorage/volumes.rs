use super::StorageActor;
use actix::{Context, Handler, ResponseActFuture};
use polymer_deno::polymer::consolestorage::*;
use polymer_library::{ConsoleStorage, StorageKind, StorageManager, VolumeInfo};
use uuid::Uuid;

impl Handler<List> for StorageActor {
    type Result = ResponseActFuture<Self, Result<Vec<StorageVolume>, StorageError>>;

    fn handle(&mut self, _: List, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |manager, _| async move {
            let storage = manager.list().await.map_err(StorageError::from_display)?;
            Ok(storage
                .iter()
                .map(|(kind, metadata)| Self::describe(&manager, *kind, metadata))
                .collect())
        })
    }
}

impl Handler<Supports> for StorageActor {
    type Result = Result<bool, StorageError>;

    fn handle(
        &mut self,
        Supports { kind, platform }: Supports,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let kind = kind
            .parse::<StorageKind>()
            .map_err(StorageError::from_display)?;
        Ok(ConsoleStorage::supports(kind, &platform))
    }
}

impl Handler<Create> for StorageActor {
    type Result = ResponseActFuture<Self, Result<StorageVolume, StorageError>>;

    fn handle(
        &mut self,
        Create {
            kind,
            platform,
            label,
        }: Create,
        _: &mut Context<Self>,
    ) -> Self::Result {
        self.execute(move |manager, _| async move {
            let kind = kind
                .parse::<StorageKind>()
                .map_err(StorageError::from_display)?;
            let storage = manager
                .create(kind, &platform, label)
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

impl Handler<Delete> for StorageActor {
    type Result = ResponseActFuture<Self, Result<(), StorageError>>;

    fn handle(
        &mut self,
        Delete { kind, platform, id }: Delete,
        _: &mut Context<Self>,
    ) -> Self::Result {
        self.execute(move |manager, _| async move {
            let _lease = polymer_vfs::ExclusiveResourceKey::Resource(Self::parse(&id)?)
                .acquire()
                .map_err(StorageError::from_display)?;
            manager
                .delete(
                    kind.parse::<StorageKind>()
                        .map_err(StorageError::from_display)?,
                    &platform,
                    Self::parse(&id)?,
                )
                .await
                .map_err(StorageError::from_display)
        })
    }
}

impl Handler<SetLabel> for StorageActor {
    type Result = ResponseActFuture<Self, Result<StorageVolume, StorageError>>;

    fn handle(
        &mut self,
        SetLabel {
            kind,
            platform,
            id,
            label,
        }: SetLabel,
        _: &mut Context<Self>,
    ) -> Self::Result {
        self.execute(move |manager, _| async move {
            let _lease = polymer_vfs::ExclusiveResourceKey::Resource(Self::parse(&id)?)
                .acquire()
                .map_err(StorageError::from_display)?;
            let kind = kind
                .parse::<StorageKind>()
                .map_err(StorageError::from_display)?;
            let storage = manager
                .set_label(kind, &platform, Self::parse(&id)?, label)
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

impl StorageActor {
    pub(super) fn parse(id: &str) -> Result<Uuid, StorageError> {
        Uuid::parse_str(id).map_err(|_| StorageError(format!("`{id}` is not a console storage id")))
    }

    fn describe(
        manager: &StorageManager,
        kind: StorageKind,
        metadata: &VolumeInfo,
    ) -> StorageVolume {
        StorageVolume {
            kind: kind.as_str().to_owned(),
            id: metadata.id.simple().to_string(),
            platform: metadata.platform.clone(),
            label: metadata.label.clone(),
            created: metadata.created.to_string(),
            path: manager
                .platform_path(&metadata.platform)
                .to_string_lossy()
                .into_owned(),
        }
    }
}
