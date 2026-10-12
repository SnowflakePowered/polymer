use super::{LibraryApi, LibraryBackend};
use crate::library::LibraryReader;
use actix::{AtomicResponse, Context, Handler, ResponseActFuture};
use base64::{Engine, engine::general_purpose::STANDARD};
use polymer_deno::polymer::library::*;
use polymer_library::MediaData;
use serde_json::Value;

impl Handler<MetadataAdd> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<Value, LibraryError>>;

    fn handle(
        &mut self,
        MetadataAdd {
            parent_binary_id,
            input,
        }: MetadataAdd,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let catalog = self.catalog.clone();
        self.mutate(async move {
            catalog
                .add_metadata(Self::uuid(&parent_binary_id)?, Self::metadata_input(input)?)
                .await
                .map(|metadata| Self::metadata_value(&metadata))
                .map_err(|error| error.to_string())
        })
    }
}

impl Handler<MetadataUpdate> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<Value, LibraryError>>;

    fn handle(
        &mut self,
        MetadataUpdate { id, input }: MetadataUpdate,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let catalog = self.catalog.clone();
        self.mutate(async move {
            catalog
                .update_metadata(Self::uuid(&id)?, Self::metadata_input(input)?)
                .await
                .map(|metadata| Self::metadata_value(&metadata))
                .map_err(|error| error.to_string())
        })
    }
}

impl Handler<MetadataDelete> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<(), LibraryError>>;

    fn handle(
        &mut self,
        MetadataDelete { id }: MetadataDelete,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let catalog = self.catalog.clone();
        self.mutate(async move {
            catalog
                .remove_metadata(Self::uuid(&id)?)
                .await
                .map_err(|error| error.to_string())
        })
    }
}

impl Handler<MetadataReplace> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<Value, LibraryError>>;

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
        self.mutate(async move {
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
                .map(|metadata| Value::Array(metadata.iter().map(Self::metadata_value).collect()))
                .map_err(|error| error.to_string())
        })
    }
}

impl Handler<MetadataText> for LibraryReader {
    type Result = ResponseActFuture<Self, Result<String, LibraryError>>;

    fn handle(&mut self, MetadataText { id }: MetadataText, _: &mut Context<Self>) -> Self::Result {
        let catalog = self.catalog.clone();
        self.read(async move {
            catalog
                .metadata_text(Self::uuid(&id)?)
                .await
                .map_err(|error| {
                    let error = error.to_string();
                    if error.contains("media metadata does not have text") {
                        LibraryError::Type(error)
                    } else {
                        LibraryError::Generic(error)
                    }
                })
        })
    }
}

impl Handler<MetadataUrl> for LibraryReader {
    type Result = ResponseActFuture<Self, Result<String, LibraryError>>;

    fn handle(&mut self, MetadataUrl { id }: MetadataUrl, _: &mut Context<Self>) -> Self::Result {
        let catalog = self.catalog.clone();
        self.read(async move {
            let id = Self::uuid(&id)?;
            let media = catalog.media(id).await.map_err(|error| {
                let error = error.to_string();
                if error.contains("text metadata does not have a URL") {
                    LibraryError::Type(error)
                } else {
                    LibraryError::Generic(error)
                }
            })?;
            match media.data {
                MediaData::Inline(bytes) => Ok::<_, LibraryError>(format!(
                    "data:{};base64,{}",
                    media.media_type,
                    STANDARD.encode(bytes)
                )),
                MediaData::Object(_) => Ok(format!(
                    "library://media/{}",
                    blake3::Hash::from_bytes(media.content_blake3).to_hex()
                )),
            }
        })
    }
}
