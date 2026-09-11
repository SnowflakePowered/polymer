use super::LibraryBackend;
use kameo::message::{Context, Message};
use polymer_deno::polymer::library::*;
use serde_json::{Value, json};

impl Message<GamesList> for LibraryBackend {
    type Reply = Result<Value, String>;

    async fn handle(
        &mut self,
        GamesList { platform }: GamesList,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.catalog
            .games(platform.as_deref())
            .await
            .map(|games| Value::Array(games.iter().map(Self::game_value).collect()))
            .map_err(|error| error.to_string())
    }
}

impl Message<GamesGet> for LibraryBackend {
    type Reply = Result<Value, String>;

    async fn handle(
        &mut self,
        GamesGet { id }: GamesGet,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.catalog
            .game(Self::uuid(&id)?)
            .await
            .map(|game| game.as_ref().map_or(Value::Null, Self::game_value))
            .map_err(|error| error.to_string())
    }
}

impl Message<GamesDelete> for LibraryBackend {
    type Reply = Result<(), String>;

    async fn handle(
        &mut self,
        GamesDelete { id }: GamesDelete,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.catalog
            .delete_game(Self::uuid(&id)?)
            .await
            .map_err(|error| error.to_string())
    }
}

impl Message<LaunchablesAdd> for LibraryBackend {
    type Reply = Result<Value, String>;

    async fn handle(
        &mut self,
        LaunchablesAdd {
            path,
            platform,
            parent_binary_id,
        }: LaunchablesAdd,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
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
}

impl Message<LaunchablesDelete> for LibraryBackend {
    type Reply = Result<(), String>;

    async fn handle(
        &mut self,
        LaunchablesDelete { id }: LaunchablesDelete,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.catalog
            .remove_launchable(Self::uuid(&id)?)
            .await
            .map_err(|error| error.to_string())
    }
}
