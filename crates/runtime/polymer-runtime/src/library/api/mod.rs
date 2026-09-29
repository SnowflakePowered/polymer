mod games;
mod import;
mod metadata;
mod scraping;

use polymer_library::{Game, Launchable, MediaData, Metadata, MetadataData, MetadataInput};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{LibraryBackend, LibraryReader};
use actix::{AtomicResponse, ResponseActFuture, WrapFuture};
use std::future::Future;

pub(crate) trait LibraryApi {
    fn uuid(value: &str) -> Result<Uuid, String> {
        if value.len() != 32
            || value
                .bytes()
                .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
        {
            return Err("UUIDs must use 32 lowercase hexadecimal characters".to_owned());
        }
        Uuid::parse_str(value).map_err(|error| format!("invalid UUID: {error}"))
    }

    fn metadata_input(value: Value) -> Result<MetadataInput, String> {
        serde_json::from_value(value).map_err(|error| format!("invalid metadata: {error}"))
    }

    fn game_value(game: &Game) -> Value {
        json!({
            "id": game.id.simple().to_string(),
            "platform": game.platform,
            "launchables": game.launchables.iter().map(Self::launchable_value).collect::<Vec<_>>(),
            "metadata": game.metadata.iter().map(Self::metadata_value).collect::<Vec<_>>(),
        })
    }

    fn launchable_value(launchable: &Launchable) -> Value {
        json!({
            "id": launchable.id.simple().to_string(),
            "binaryId": launchable.binary_id.simple().to_string(),
            "uri": launchable.path,
            "serial": launchable.serial,
            "titleId": launchable.title_id,
            "parentBinaryId": launchable.parent_binary_id.map(|id| id.simple().to_string()),
            "platform": launchable.platform,
            "preparation": launchable.manifest.as_ref().map(|manifest| json!({
                "state": if manifest.template.is_some() { "ready" } else if manifest.error.is_some() { "failed" } else { "pending" },
                "error": manifest.error,
            })),
        })
    }

    fn metadata_value(metadata: &Metadata) -> Value {
        let data = match &metadata.data {
            MetadataData::Scalar(value) => json!({ "kind": "scalar", "value": value }),
            MetadataData::Media {
                media_type,
                data,
                source_url,
            } => json!({
                "kind": "media",
                "mediaType": media_type,
                "sourceUrl": source_url,
                "storage": match data {
                    MediaData::Inline(_) => "inline",
                    MediaData::Object(_) => "object",
                },
            }),
        };
        json!({
            "id": metadata.id.simple().to_string(),
            "parentBinaryId": metadata.parent_binary_id.simple().to_string(),
            "key": metadata.key,
            "source": metadata.source,
            "producer": metadata.producer,
            "ordinal": metadata.ordinal,
            "data": data,
        })
    }
}

impl LibraryApi for LibraryBackend {}
impl LibraryApi for LibraryReader {}

impl LibraryReader {
    fn read<T: 'static>(
        &self,
        future: impl Future<Output = Result<T, String>> + 'static,
    ) -> ResponseActFuture<Self, Result<T, String>> {
        Box::pin(future.into_actor(self))
    }
}

impl LibraryBackend {
    fn mutate<T: 'static>(
        &self,
        future: impl Future<Output = Result<T, String>> + 'static,
    ) -> AtomicResponse<Self, Result<T, String>> {
        AtomicResponse::new(Box::pin(future.into_actor(self)))
    }
}
