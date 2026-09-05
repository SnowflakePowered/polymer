use std::{future::Future, pin::Pin};

use polymer_command::command_handler;
use polymer_deno::polymer::LibraryCommandHandler;
use polymer_library::{Game, Launchable, MediaData, Metadata, MetadataData, MetadataInput};
use serde_json::{Value, json};
use uuid::Uuid;

use super::LibraryBackend;

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
        tracing::info!(%path, "identifying dropped game for library import");
        let package = tokio::task::spawn_blocking(move || {
            polymer_scraping::Package::identify(path, platform.as_deref(), parent)
        })
        .await
        .map_err(|error| format!("package detection task failed: {error}"))?
        .map_err(|error| error.to_string())?;
        let (launchable, created_root) = package
            .import(&self.catalog)
            .await
            .map_err(|error| error.to_string())?;
        let is_root = launchable.parent_binary_id.is_none();
        let root_binary_id = launchable.parent_binary_id.unwrap_or(launchable.binary_id);
        tracing::info!(root = %root_binary_id, path = ?launchable.path, created_root, "game imported into library");
        self.scraping.publish_library_content(root_binary_id);
        let job = if is_root && created_root {
            Some(
                self.scraping
                    .enqueue(launchable.binary_id, launchable.binary_id)
                    .await
                    .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        Ok(json!({ "launchable": Self::launchable_value(&launchable), "scrapeJob": job }))
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
            .replace_metadata(
                Self::uuid(&parent_binary_id)?,
                &format!("manual:{source}"),
                inputs,
            )
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

    async fn scraping_jobs(&mut self) -> Result<Value, String> {
        self.catalog
            .scrape_jobs()
            .await
            .map(|jobs| json!(jobs))
            .map_err(|error| error.to_string())
    }

    async fn scraping_start(
        &mut self,
        root_binary_id: String,
        trigger_binary_id: String,
    ) -> Result<Value, String> {
        self.scraping
            .enqueue(
                Self::uuid(&root_binary_id)?,
                Self::uuid(&trigger_binary_id)?,
            )
            .await
            .map(|job| json!(job))
            .map_err(|error| error.to_string())
    }

    async fn scraping_job(&mut self, id: String) -> Result<Value, String> {
        self.catalog
            .scrape_job(Self::uuid(&id)?)
            .await
            .map(|job| json!(job))
            .map_err(|error| error.to_string())
    }

    async fn scraping_retry(&mut self, id: String) -> Result<Value, String> {
        self.scraping
            .retry(Self::uuid(&id)?)
            .await
            .map(|job| json!(job))
            .map_err(|error| error.to_string())
    }

    async fn scraping_scrapers(&mut self) -> Result<Value, String> {
        Ok(Value::Array(self.scraping.scrapers().iter().map(|plugin| json!({
            "id": plugin.id(),
            "version": plugin.version(),
            "requires": plugin.requirements().iter().map(ToString::to_string).collect::<Vec<_>>(),
            "provides": plugin.provides(),
        })).collect()))
    }

    async fn scraping_refresh(&mut self, plugin: String) -> Result<(), String> {
        self.scraping
            .refresh(&plugin)
            .await
            .map_err(|error| error.to_string())
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

    pub(super) fn metadata_input(value: Value) -> Result<MetadataInput, String> {
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
