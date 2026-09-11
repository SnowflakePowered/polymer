use super::LibraryBackend;
use kameo::message::{Context, Message};
use polymer_deno::polymer::library::*;
use serde_json::Value;

impl Message<MetadataAdd> for LibraryBackend {
    type Reply = Result<Value, String>;

    async fn handle(
        &mut self,
        MetadataAdd {
            parent_binary_id,
            input,
        }: MetadataAdd,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.catalog
            .add_metadata(Self::uuid(&parent_binary_id)?, Self::metadata_input(input)?)
            .await
            .map(|metadata| Self::metadata_value(&metadata))
            .map_err(|error| error.to_string())
    }
}

impl Message<MetadataUpdate> for LibraryBackend {
    type Reply = Result<Value, String>;

    async fn handle(
        &mut self,
        MetadataUpdate { id, input }: MetadataUpdate,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.catalog
            .update_metadata(Self::uuid(&id)?, Self::metadata_input(input)?)
            .await
            .map(|metadata| Self::metadata_value(&metadata))
            .map_err(|error| error.to_string())
    }
}

impl Message<MetadataDelete> for LibraryBackend {
    type Reply = Result<(), String>;

    async fn handle(
        &mut self,
        MetadataDelete { id }: MetadataDelete,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.catalog
            .remove_metadata(Self::uuid(&id)?)
            .await
            .map_err(|error| error.to_string())
    }
}

impl Message<MetadataReplace> for LibraryBackend {
    type Reply = Result<Value, String>;

    async fn handle(
        &mut self,
        MetadataReplace {
            parent_binary_id,
            source,
            inputs,
        }: MetadataReplace,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        let inputs = inputs
            .into_iter()
            .map(Self::metadata_input)
            .collect::<Result<Vec<_>, _>>()?;
        self.catalog
            .replace_metadata(
                Self::uuid(&parent_binary_id)?,
                &format!("manual:{source}"),
                inputs,
            )
            .await
            .map(|metadata| Value::Array(metadata.iter().map(Self::metadata_value).collect()))
            .map_err(|error| error.to_string())
    }
}

impl Message<MetadataText> for LibraryBackend {
    type Reply = Result<String, String>;

    async fn handle(
        &mut self,
        MetadataText { id }: MetadataText,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.catalog
            .metadata_text(Self::uuid(&id)?)
            .await
            .map_err(|error| {
                let error = error.to_string();
                if error.contains("media metadata does not have text") {
                    format!("TypeError: {error}")
                } else {
                    error
                }
            })
    }
}

impl Message<MetadataUrl> for LibraryBackend {
    type Reply = Result<String, String>;

    async fn handle(
        &mut self,
        MetadataUrl { id }: MetadataUrl,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.media_url(Self::uuid(&id)?).await
    }
}
