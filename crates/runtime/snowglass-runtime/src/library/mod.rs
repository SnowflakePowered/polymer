mod api;
mod server;

use std::sync::Arc;

use base64::{Engine, engine::general_purpose::STANDARD};
use snowglass_deno::snowglass::Library;
use snowglass_library::{Directories, GameCatalog, MediaData};

#[derive(Clone)]
pub struct LibraryService {
    client: Library,
}

struct LibraryBackend {
    catalog: Arc<GameCatalog>,
    media: server::MediaServer,
}

impl LibraryService {
    pub async fn open(directories: &Directories) -> Result<Self, Box<dyn std::error::Error>> {
        let catalog = Arc::new(GameCatalog::open_local(directories).await?);
        Ok(Self::start(catalog).await?)
    }

    async fn start(catalog: Arc<GameCatalog>) -> Result<Self, std::io::Error> {
        let media = server::MediaServer::start(catalog.clone()).await?;
        let (client, mut commands) = Library::channel();
        let mut backend = LibraryBackend { catalog, media };
        tokio::spawn(async move {
            while let Some(command) = commands.recv().await {
                command.dispatch(&mut backend).await;
            }
        });
        Ok(Self { client })
    }

    pub fn client(&self) -> Library {
        self.client.clone()
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
    use snowglass_library::{IdentifiedLaunchable, MetadataInput, Relation};

    use super::*;

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
        let service = LibraryService::start(catalog.clone()).await.unwrap();
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
                MetadataInput::Inline {
                    relation: Relation::Icon,
                    language: None,
                    source: "banner".to_owned(),
                    media_type: "image/gif".to_owned(),
                    bytes: Box::from(&b"GIF89a"[..]),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            service
                .client()
                .metadata_url(inline.id.simple().to_string())
                .await
                .unwrap(),
            "data:image/gif;base64,R0lGODlh"
        );

        let source = directory.path().join("video.bin");
        tokio::fs::write(&source, b"0123456789").await.unwrap();
        let object = catalog
            .add_metadata(
                launchable.binary_id,
                MetadataInput::Object {
                    relation: Relation::Video,
                    language: None,
                    source: "test".to_owned(),
                    media_type: "video/mp4".to_owned(),
                    path: source.clone(),
                },
            )
            .await
            .unwrap();
        let old_url = service
            .client()
            .metadata_url(object.id.simple().to_string())
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
                MetadataInput::Object {
                    relation: Relation::Video,
                    language: None,
                    source: "test".to_owned(),
                    media_type: "video/mp4".to_owned(),
                    path: directory.path().join("video.bin"),
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
