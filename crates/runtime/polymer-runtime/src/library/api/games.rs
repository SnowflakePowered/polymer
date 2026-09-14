use super::{LibraryApi, LibraryBackend};
use crate::library::LibraryReader;
use actix::{AtomicResponse, Context, Handler, ResponseActFuture};
use polymer_deno::polymer::library::*;
use serde_json::{Value, json};

impl Handler<GamesList> for LibraryReader {
    type Result = ResponseActFuture<Self, Result<Value, String>>;

    fn handle(&mut self, GamesList { platform }: GamesList, _: &mut Context<Self>) -> Self::Result {
        let catalog = self.catalog.clone();
        self.read(async move {
            catalog
                .games(platform.as_deref())
                .await
                .map(|games| Value::Array(games.iter().map(Self::game_value).collect()))
                .map_err(|error| error.to_string())
        })
    }
}

impl Handler<GamesGet> for LibraryReader {
    type Result = ResponseActFuture<Self, Result<Value, String>>;

    fn handle(&mut self, GamesGet { id }: GamesGet, _: &mut Context<Self>) -> Self::Result {
        let catalog = self.catalog.clone();
        self.read(async move {
            catalog
                .game(Self::uuid(&id)?)
                .await
                .map(|game| game.as_ref().map_or(Value::Null, Self::game_value))
                .map_err(|error| error.to_string())
        })
    }
}

impl Handler<GamesDelete> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<(), String>>;

    fn handle(&mut self, GamesDelete { id }: GamesDelete, _: &mut Context<Self>) -> Self::Result {
        let catalog = self.catalog.clone();
        self.mutate(async move {
            catalog
                .delete_game(Self::uuid(&id)?)
                .await
                .map_err(|error| error.to_string())
        })
    }
}

impl Handler<LaunchablesAdd> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<Value, String>>;

    fn handle(
        &mut self,
        LaunchablesAdd {
            path,
            platform,
            parent_binary_id,
        }: LaunchablesAdd,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let scraping = self.scraping.clone();
        self.mutate(async move {
            let parent = parent_binary_id.as_deref().map(Self::uuid).transpose()?;
            let packages = tokio::task::spawn_blocking(move || {
                polymer_scraping::Ingestor::identify(
                    std::path::Path::new(&path),
                    platform.as_deref(),
                    parent,
                )
            })
            .await
            .map_err(|error| format!("package detection task failed: {error}"))?
            .map_err(|error| error.to_string())?;
            let imported = polymer_scraping::Ingestor::import(&scraping, packages)
                .await
                .map_err(|error| error.to_string())?;
            let imports: Vec<_> = imported
                .iter()
                .map(|(launchable, job)| {
                    json!({
                        "launchable": Self::launchable_value(launchable), "scrapeJob": job,
                    })
                })
                .collect();
            // Preserve the single-import response while exposing every collection entry.
            let mut response = imports.first().cloned().ok_or("no packages found")?;
            response["imports"] = json!(imports);
            Ok(response)
        })
    }
}

impl Handler<LaunchablesDelete> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<(), String>>;

    fn handle(
        &mut self,
        LaunchablesDelete { id }: LaunchablesDelete,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let catalog = self.catalog.clone();
        self.mutate(async move {
            catalog
                .remove_launchable(Self::uuid(&id)?)
                .await
                .map_err(|error| error.to_string())
        })
    }
}
