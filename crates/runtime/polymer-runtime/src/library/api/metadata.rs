use super::LibraryBackend;
use actix::{AtomicResponse, Context, Handler, WrapFuture};
use base64::{Engine, engine::general_purpose::STANDARD};
use polymer_deno::polymer::library::*;
use polymer_library::MediaData;
use serde_json::Value;

impl Handler<MetadataAdd> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<Value, String>>;

    fn handle(
        &mut self,
        MetadataAdd {
            parent_binary_id,
            input,
        }: MetadataAdd,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let catalog = self.catalog.clone();
        AtomicResponse::new(Box::pin(
            async move {
                catalog
                    .add_metadata(Self::uuid(&parent_binary_id)?, Self::metadata_input(input)?)
                    .await
                    .map(|metadata| Self::metadata_value(&metadata))
                    .map_err(|error| error.to_string())
            }
            .into_actor(self),
        ))
    }
}

impl Handler<MetadataUpdate> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<Value, String>>;

    fn handle(
        &mut self,
        MetadataUpdate { id, input }: MetadataUpdate,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let catalog = self.catalog.clone();
        AtomicResponse::new(Box::pin(
            async move {
                catalog
                    .update_metadata(Self::uuid(&id)?, Self::metadata_input(input)?)
                    .await
                    .map(|metadata| Self::metadata_value(&metadata))
                    .map_err(|error| error.to_string())
            }
            .into_actor(self),
        ))
    }
}

impl Handler<MetadataDelete> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<(), String>>;

    fn handle(
        &mut self,
        MetadataDelete { id }: MetadataDelete,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let catalog = self.catalog.clone();
        AtomicResponse::new(Box::pin(
            async move {
                catalog
                    .remove_metadata(Self::uuid(&id)?)
                    .await
                    .map_err(|error| error.to_string())
            }
            .into_actor(self),
        ))
    }
}

impl Handler<MetadataReplace> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<Value, String>>;

    fn handle(
        &mut self,
        MetadataReplace {
            parent_binary_id,
            source,
            inputs,
        }: MetadataReplace,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let catalog = self.catalog.clone();
        AtomicResponse::new(Box::pin(
            async move {
                let inputs = inputs
                    .into_iter()
                    .map(Self::metadata_input)
                    .collect::<Result<Vec<_>, _>>()?;
                catalog
                    .replace_metadata(
                        Self::uuid(&parent_binary_id)?,
                        &format!("manual:{source}"),
                        inputs,
                    )
                    .await
                    .map(|metadata| {
                        Value::Array(metadata.iter().map(Self::metadata_value).collect())
                    })
                    .map_err(|error| error.to_string())
            }
            .into_actor(self),
        ))
    }
}

impl Handler<MetadataText> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<String, String>>;

    fn handle(&mut self, MetadataText { id }: MetadataText, _: &mut Context<Self>) -> Self::Result {
        let catalog = self.catalog.clone();
        AtomicResponse::new(Box::pin(
            async move {
                catalog
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
            .into_actor(self),
        ))
    }
}

impl Handler<MetadataUrl> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<String, String>>;

    fn handle(&mut self, MetadataUrl { id }: MetadataUrl, _: &mut Context<Self>) -> Self::Result {
        let catalog = self.catalog.clone();
        let server = self.media.clone();
        AtomicResponse::new(Box::pin(
            async move {
                let id = Self::uuid(&id)?;
                let media = catalog.media(id).await.map_err(|error| {
                    let error = error.to_string();
                    if error.contains("text metadata does not have a URL") {
                        format!("TypeError: {error}")
                    } else {
                        error
                    }
                })?;
                match media.data {
                    MediaData::Inline(bytes) => Ok(format!(
                        "data:{};base64,{}",
                        media.media_type,
                        STANDARD.encode(bytes)
                    )),
                    MediaData::Object(_) => {
                        let hash = blake3::Hash::from_bytes(media.content_blake3).to_hex();
                        Ok(server.url(media.metadata_id, hash.as_str()))
                    }
                }
            }
            .into_actor(self),
        ))
    }
}
