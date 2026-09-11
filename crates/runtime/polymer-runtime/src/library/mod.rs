mod api;
mod server;

use std::sync::Arc;

use base64::{Engine, engine::general_purpose::STANDARD};
use kameo::{
    Actor,
    actor::{ActorRef, Spawn},
};
use polymer_deno::polymer::library::Library;
use polymer_library::{Directories, GameCatalog, MediaData};

#[derive(Clone)]
pub struct LibraryService {
    configuration_locks: crate::launchable_configuration::ConfigurationLocks,
    catalog: Arc<GameCatalog>,
    actor: ActorRef<LibraryBackend>,
    scraping: polymer_scraping::ScrapingEngine,
}

struct LibraryBackend {
    catalog: Arc<GameCatalog>,
    scraping: polymer_scraping::ScrapingEngine,
    media: server::MediaServer,
}

impl Actor for LibraryBackend {
    type Args = Self;
    type Error = std::convert::Infallible;

    async fn on_start(state: Self, _: ActorRef<Self>) -> Result<Self, Self::Error> {
        Ok(state)
    }
}

impl LibraryService {
    pub async fn open(directories: &Directories) -> Result<Self, Box<dyn std::error::Error>> {
        Self::open_with_plugins(
            directories,
            polymer_pluginhost::loading::PluginRegistry::new(),
        )
        .await
    }

    pub async fn open_with_plugins(
        directories: &Directories,
        plugins: polymer_pluginhost::loading::PluginRegistry,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let catalog = Arc::new(GameCatalog::open_local(directories).await?);
        Ok(Self::start(catalog, plugins, directories.clone()).await?)
    }

    async fn start(
        catalog: Arc<GameCatalog>,
        plugins: polymer_pluginhost::loading::PluginRegistry,
        directories: Directories,
    ) -> Result<Self, std::io::Error> {
        let media = server::MediaServer::start(catalog.clone()).await?;
        let scraping = polymer_scraping::ScrapingEngine::new(catalog.clone(), plugins, directories);
        if let Err(error) = scraping.resume().await {
            tracing::warn!(%error, "could not resume scrape jobs");
        }
        let backend = LibraryBackend {
            catalog: catalog.clone(),
            scraping: scraping.clone(),
            media,
        };
        let actor = LibraryBackend::spawn(backend);

        Ok(Self {
            configuration_locks: Default::default(),
            actor,
            scraping,
            catalog,
        })
    }

    pub fn client(&self) -> Library {
        Library {
            games_list: self.actor.clone().reply_recipient(),
            games_get: self.actor.clone().reply_recipient(),
            games_delete: self.actor.clone().reply_recipient(),
            launchables_add: self.actor.clone().reply_recipient(),
            launchables_delete: self.actor.clone().reply_recipient(),
            metadata_add: self.actor.clone().reply_recipient(),
            metadata_update: self.actor.clone().reply_recipient(),
            metadata_delete: self.actor.clone().reply_recipient(),
            metadata_replace: self.actor.clone().reply_recipient(),
            metadata_text: self.actor.clone().reply_recipient(),
            metadata_url: self.actor.clone().reply_recipient(),
            scraping_jobs: self.actor.clone().reply_recipient(),
            scraping_start: self.actor.clone().reply_recipient(),
            scraping_job: self.actor.clone().reply_recipient(),
            scraping_retry: self.actor.clone().reply_recipient(),
            scraping_scrapers: self.actor.clone().reply_recipient(),
            scraping_refresh: self.actor.clone().reply_recipient(),
        }
    }

    pub(crate) fn catalog(&self) -> Arc<GameCatalog> {
        self.catalog.clone()
    }

    pub(crate) fn configuration_locks(
        &self,
    ) -> crate::launchable_configuration::ConfigurationLocks {
        self.configuration_locks.clone()
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<polymer_scraping::ScrapingEvent> {
        self.scraping.subscribe()
    }
}

impl LibraryBackend {
    async fn media_url(&self, id: uuid::Uuid) -> Result<String, String> {
        let media = self.catalog.media(id).await.map_err(|error| {
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
                Ok(self.media.url(media.metadata_id, hash.as_str()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use object_store::memory::InMemory;
    use polymer_library::{IdentifiedLaunchable, MetadataInput, MetadataInputData};

    use super::*;
    use polymer_deno::polymer::library::MetadataUrl;

    #[test]
    fn metadata_wire_value_deserializes_into_the_catalog_input() {
        let input = LibraryBackend::metadata_input(serde_json::json!({
            "key": "meta.score",
            "source": "test",
            "kind": "scalar",
            "value": 9.5,
        }))
        .unwrap();
        assert_eq!(
            input,
            MetadataInput {
                key: "meta.score".to_owned(),
                source: "test".to_owned(),
                data: MetadataInputData::Scalar {
                    value: polymer_library::MetadataValue::Number(9.5),
                },
            }
        );
    }

    #[tokio::test]
    async fn urls_inline_data_and_protect_current_object_content() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = Arc::new(
            GameCatalog::open(
                directory.path().join("library.db"),
                Arc::new(InMemory::new()),
            )
            .await
            .unwrap(),
        );
        let service = LibraryService::start(
            catalog.clone(),
            polymer_pluginhost::loading::PluginRegistry::new(),
            Directories::rooted(directory.path(), directory.path()),
        )
        .await
        .unwrap();
        let launchable = catalog
            .import_launchable(IdentifiedLaunchable {
                path: directory.path().join("game.rom"),
                binary_blake3: *blake3::hash(b"game").as_bytes(),
                serial: None,
                title_id: None,
                parent_binary_id: None,
                platform: "NINTENDO_NES".to_owned(),
            })
            .await
            .unwrap();
        let inline = catalog
            .add_metadata(
                launchable.binary_id,
                MetadataInput {
                    key: "media.icon".to_owned(),
                    source: "banner".to_owned(),
                    data: MetadataInputData::Inline {
                        media_type: "image/gif".to_owned(),
                        bytes: Box::from(&b"GIF89a"[..]),
                        source_url: None,
                    },
                },
            )
            .await
            .unwrap();
        assert_eq!(
            service
                .actor
                .ask(MetadataUrl {
                    id: inline.id.simple().to_string()
                })
                .send()
                .await
                .unwrap(),
            "data:image/gif;base64,R0lGODlh"
        );

        let source = directory.path().join("video.bin");
        tokio::fs::write(&source, b"0123456789").await.unwrap();
        let object = catalog
            .add_metadata(
                launchable.binary_id,
                MetadataInput {
                    key: "media.video".to_owned(),
                    source: "test".to_owned(),
                    data: MetadataInputData::Object {
                        media_type: "video/mp4".to_owned(),
                        path: source.clone(),
                        source_url: None,
                    },
                },
            )
            .await
            .unwrap();
        let old_url = service
            .actor
            .ask(MetadataUrl {
                id: object.id.simple().to_string(),
            })
            .send()
            .await
            .unwrap();
        let response = reqwest::Client::new()
            .get(&old_url)
            .header("Range", "bytes=2-5")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()["Content-Range"], "bytes 2-5/10");
        assert_eq!(response.bytes().await.unwrap(), &b"2345"[..]);

        tokio::fs::write(source, b"replacement").await.unwrap();
        catalog
            .update_metadata(
                object.id,
                MetadataInput {
                    key: "media.video".to_owned(),
                    source: "test".to_owned(),
                    data: MetadataInputData::Object {
                        media_type: "video/mp4".to_owned(),
                        path: directory.path().join("video.bin"),
                        source_url: None,
                    },
                },
            )
            .await
            .unwrap();
        assert_eq!(
            reqwest::get(old_url).await.unwrap().status(),
            reqwest::StatusCode::NOT_FOUND
        );
    }
}
