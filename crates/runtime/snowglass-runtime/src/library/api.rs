use std::{future::Future, path::PathBuf, pin::Pin};

use serde::Deserialize;
use serde_json::{Value, json};
use snowglass_command::command_handler;
use snowglass_deno::snowglass::LibraryCommandHandler;
use snowglass_library::{
    Game, Launchable, MediaData, Metadata, MetadataData, MetadataInput, Relation,
};
use uuid::Uuid;

use super::LibraryBackend;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetadataWrite {
    relation: Relation,
    language: Option<String>,
    source: String,
    #[serde(flatten)]
    data: MetadataWriteData,
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
enum MetadataWriteData {
    Text { value: String },
    Year { value: u16 },
    Inline { media_type: String, bytes: Vec<u8> },
    Object { media_type: String, path: PathBuf },
}

#[command_handler]
impl LibraryCommandHandler for LibraryBackend {
    type Output<'handler>
        = Pin<Box<dyn Future<Output = ()> + Send + 'handler>>
    where
        Self: 'handler;

    async fn games_list(&mut self, platform: Option<String>) -> Result<Value, String> {
        self.catalog
            .games(platform.as_deref())
            .await
            .map(|games| Value::Array(games.iter().map(Self::game_value).collect()))
            .map_err(|error| error.to_string())
    }

    async fn games_get(&mut self, id: String) -> Result<Value, String> {
        self.catalog
            .game(Self::uuid(&id)?)
            .await
            .map(|game| game.as_ref().map_or(Value::Null, Self::game_value))
            .map_err(|error| error.to_string())
    }

    async fn games_delete(&mut self, id: String) -> Result<(), String> {
        self.catalog
            .delete_game(Self::uuid(&id)?)
            .await
            .map_err(|error| error.to_string())
    }

    async fn launchables_add(
        &mut self,
        path: String,
        platform: Option<String>,
        parent_binary_id: Option<String>,
    ) -> Result<Value, String> {
        let parent = parent_binary_id.as_deref().map(Self::uuid).transpose()?;
        let identified = tokio::task::spawn_blocking(move || {
            crate::ScrapedPackage::identify_launchable(path, platform.as_deref(), parent)
        })
        .await
        .map_err(|error| format!("package detection task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        self.catalog
            .import_launchable(identified)
            .await
            .map(|launchable| Self::launchable_value(&launchable))
            .map_err(|error| error.to_string())
    }

    async fn launchables_delete(&mut self, id: String) -> Result<(), String> {
        self.catalog
            .remove_launchable(Self::uuid(&id)?)
            .await
            .map_err(|error| error.to_string())
    }

    async fn metadata_add(
        &mut self,
        parent_binary_id: String,
        input: Value,
    ) -> Result<Value, String> {
        self.catalog
            .add_metadata(Self::uuid(&parent_binary_id)?, Self::metadata_input(input)?)
            .await
            .map(|metadata| Self::metadata_value(&metadata))
            .map_err(|error| error.to_string())
    }

    async fn metadata_update(&mut self, id: String, input: Value) -> Result<Value, String> {
        self.catalog
            .update_metadata(Self::uuid(&id)?, Self::metadata_input(input)?)
            .await
            .map(|metadata| Self::metadata_value(&metadata))
            .map_err(|error| error.to_string())
    }

    async fn metadata_delete(&mut self, id: String) -> Result<(), String> {
        self.catalog
            .remove_metadata(Self::uuid(&id)?)
            .await
            .map_err(|error| error.to_string())
    }

    async fn metadata_replace(
        &mut self,
        parent_binary_id: String,
        source: String,
        inputs: Vec<Value>,
    ) -> Result<Value, String> {
        let inputs = inputs
            .into_iter()
            .map(Self::metadata_input)
            .collect::<Result<Vec<_>, _>>()?;
        self.catalog
            .replace_metadata(Self::uuid(&parent_binary_id)?, &source, inputs)
            .await
            .map(|metadata| Value::Array(metadata.iter().map(Self::metadata_value).collect()))
            .map_err(|error| error.to_string())
    }

    async fn metadata_text(&mut self, id: String) -> Result<String, String> {
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

    async fn metadata_url(&mut self, id: String) -> Result<String, String> {
        self.media_url(Self::uuid(&id)?).await
    }
}

impl LibraryBackend {
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
        let value: MetadataWrite =
            serde_json::from_value(value).map_err(|error| format!("invalid metadata: {error}"))?;
        Ok(match value.data {
            MetadataWriteData::Text { value: text } => MetadataInput::Text {
                relation: value.relation,
                language: value.language,
                source: value.source,
                value: text,
            },
            MetadataWriteData::Year { value: year } => MetadataInput::ReleaseYear {
                language: value.language,
                source: value.source,
                year,
            },
            MetadataWriteData::Inline { media_type, bytes } => MetadataInput::Inline {
                relation: value.relation,
                language: value.language,
                source: value.source,
                media_type,
                bytes: bytes.into_boxed_slice(),
            },
            MetadataWriteData::Object { media_type, path } => MetadataInput::Object {
                relation: value.relation,
                language: value.language,
                source: value.source,
                media_type,
                path,
            },
        })
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
            "path": launchable.path,
            "binaryBlake3": launchable.binary_blake3.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
            "serial": launchable.serial,
            "titleId": launchable.title_id,
            "parentBinaryId": launchable.parent_binary_id.map(|id| id.simple().to_string()),
            "platform": launchable.platform,
        })
    }

    fn metadata_value(metadata: &Metadata) -> Value {
        let data = match &metadata.data {
            MetadataData::Text(_) => json!({ "kind": "text" }),
            MetadataData::ReleaseYear(_) => json!({ "kind": "year" }),
            MetadataData::Media { media_type, data } => json!({
                "kind": "media",
                "mediaType": media_type,
                "storage": match data {
                    MediaData::Inline(_) => "inline",
                    MediaData::Object(_) => "object",
                },
            }),
        };
        json!({
            "id": metadata.id.simple().to_string(),
            "parentBinaryId": metadata.parent_binary_id.simple().to_string(),
            "relation": metadata.relation.as_str(),
            "language": metadata.language,
            "source": metadata.source,
            "data": data,
        })
    }
}
