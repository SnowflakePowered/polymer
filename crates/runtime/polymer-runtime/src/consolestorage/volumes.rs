use super::StorageActor;
use actix::{Context, Handler, ResponseActFuture};
use polymer_deno::polymer::consolestorage::*;
use polymer_library::{ConsoleStorage, StorageKind, StorageManager, VolumeInfo};
use serde_json::{Value, json};
use uuid::Uuid;

impl Handler<List> for StorageActor {
    type Result = ResponseActFuture<Self, Result<Value, String>>;

    fn handle(&mut self, _: List, _: &mut Context<Self>) -> Self::Result {
        self.execute(move |manager, _| async move {
            let storage = manager.list().await.map_err(|error| error.to_string())?;
            Ok(Value::Array(
                storage
                    .iter()
                    .map(|(kind, metadata)| Self::describe(&manager, *kind, metadata))
                    .collect(),
            ))
        })
    }
}

impl Handler<Supports> for StorageActor {
    type Result = Result<Value, String>;

    fn handle(
        &mut self,
        Supports { kind, platform }: Supports,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let kind = kind
            .parse::<StorageKind>()
            .map_err(|error| error.to_string())?;
        Ok(Value::Bool(ConsoleStorage::supports(kind, &platform)))
    }
}

impl Handler<Create> for StorageActor {
    type Result = ResponseActFuture<Self, Result<Value, String>>;

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
                .map_err(|error| error.to_string())?;
            let storage = manager
                .create(kind, &platform, label)
                .await
                .map_err(|error| error.to_string())?;
            let metadata = storage
                .metadata()
                .await
                .map_err(|error| error.to_string())?;
            Ok(Self::describe(&manager, kind, &metadata))
        })
    }
}

impl Handler<Delete> for StorageActor {
    type Result = ResponseActFuture<Self, Result<Value, String>>;

    fn handle(
        &mut self,
        Delete { kind, platform, id }: Delete,
        _: &mut Context<Self>,
    ) -> Self::Result {
        self.execute(move |manager, _| async move {
            let _lease = polymer_vfs::ExclusiveResourceKey::Resource(Self::parse(&id)?)
                .acquire()
                .map_err(|error| error.to_string())?;
            manager
                .delete(
                    kind.parse::<StorageKind>()
                        .map_err(|error| error.to_string())?,
                    &platform,
                    Self::parse(&id)?,
                )
                .await
                .map(|()| Value::Null)
                .map_err(|error| error.to_string())
        })
    }
}

impl Handler<SetLabel> for StorageActor {
    type Result = ResponseActFuture<Self, Result<Value, String>>;

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
                .map_err(|error| error.to_string())?;
            let kind = kind
                .parse::<StorageKind>()
                .map_err(|error| error.to_string())?;
            let storage = manager
                .set_label(kind, &platform, Self::parse(&id)?, label)
                .await
                .map_err(|error| error.to_string())?;
            let metadata = storage
                .metadata()
                .await
                .map_err(|error| error.to_string())?;
            Ok(Self::describe(&manager, kind, &metadata))
        })
    }
}

impl StorageActor {
    pub(super) fn parse(id: &str) -> Result<Uuid, String> {
        Uuid::parse_str(id).map_err(|_| format!("`{id}` is not a console storage id"))
    }

    fn describe(manager: &StorageManager, kind: StorageKind, metadata: &VolumeInfo) -> Value {
        json!({
            "kind": kind.as_str(),
            "id": metadata.id.simple().to_string(),
            "platform": metadata.platform,
            "label": metadata.label,
            "created": metadata.created.to_string(),
            "path": manager.platform_path(&metadata.platform).to_string_lossy(),
        })
    }
}
