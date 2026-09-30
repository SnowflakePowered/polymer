use super::{LibraryApi, LibraryBackend};
use crate::library::LibraryReader;
use actix::{AtomicResponse, Context, Handler, ResponseActFuture};
use polymer_deno::polymer::library::*;
use serde_json::{Value, json};

impl Handler<GamesList> for LibraryReader {
    type Result = ResponseActFuture<Self, Result<Value, LibraryError>>;

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
    type Result = ResponseActFuture<Self, Result<Value, LibraryError>>;

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

impl Handler<LaunchablesFind> for LibraryReader {
    type Result = ResponseActFuture<Self, Result<Value, LibraryError>>;

    fn handle(
        &mut self,
        LaunchablesFind { uri }: LaunchablesFind,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let catalog = self.catalog.clone();
        self.read(async move {
            let uri = uri
                .parse::<polymer_library::LaunchableUri>()
                .map_err(|error| error.to_string())?;
            catalog
                .launchable_at(&uri)
                .await
                .map(|launchable| {
                    launchable
                        .as_ref()
                        .map_or(Value::Null, Self::launchable_value)
                })
                .map_err(|error| error.to_string())
        })
    }
}

impl Handler<GamesDelete> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<(), LibraryError>>;

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
    type Result = AtomicResponse<Self, Result<Value, LibraryError>>;

    fn handle(
        &mut self,
        LaunchablesAdd {
            path,
            platform,
            parent_binary_id,
            artifact,
        }: LaunchablesAdd,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let scraping = self.scraping.clone();
        let catalog = self.catalog.clone();
        self.mutate(async move {
            let parent = parent_binary_id.as_deref().map(Self::uuid).transpose()?;
            let installed = artifact
                .map(|artifact| {
                    let platform = platform
                        .clone()
                        .ok_or("an installed artifact requires its platform")?;
                    Ok::<_, String>((
                        polymer_library::LaunchableUri::artifact(
                            platform,
                            Self::uuid(&artifact.storage)?,
                            artifact.id,
                        ),
                        artifact.entrypoint,
                        artifact.package,
                    ))
                })
                .transpose()?;
            let source = std::path::PathBuf::from(path);
            let identified = installed.as_ref().map(|(uri, ..)| uri.clone());
            let identify = source.clone();
            let mut packages = tokio::task::spawn_blocking(move || {
                let path = match identified {
                    Some(uri) => uri.package_path()?,
                    None => identify,
                };
                polymer_scraping::Ingestor::identify(&path, platform.as_deref(), parent)
                    .map_err(|error| std::io::Error::other(error.to_string()))
            })
            .await
            .map_err(|error| format!("package detection task failed: {error}"))?
            .map_err(|error| error.to_string())?;
            let depot = match (installed, packages.as_mut_slice()) {
                (None, _) => None,
                (Some((uri, entrypoint, package_root)), [package]) => {
                    package.install(uri, entrypoint, package_root, &source);
                    Some(
                        polymer_library::LaunchableUri::file(&source)
                            .map_err(|error| error.to_string())?,
                    )
                }
                (Some(_), _) => {
                    return Err("an installed artifact must identify as one package".to_owned());
                }
            };
            let imports = Self::import_games(&scraping, packages).await?;
            if let Some(depot) = depot
                && let Some(launchable) = catalog
                    .launchable_at(&depot)
                    .await
                    .map_err(|error| error.to_string())?
            {
                catalog
                    .remove_launchable(launchable.id)
                    .await
                    .map_err(|error| error.to_string())?;
            }
            // Preserve the single-import response while exposing every collection entry.
            let mut response = imports.first().cloned().ok_or("no packages found")?;
            response["imports"] = json!(imports);
            Ok(response)
        })
    }
}

impl Handler<LaunchablesDelete> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<(), LibraryError>>;

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

impl LibraryBackend {
    pub(super) async fn import_games(
        scraping: &polymer_scraping::ScrapingEngine,
        packages: Vec<polymer_scraping::Package>,
    ) -> Result<Vec<Value>, String> {
        let imported = polymer_scraping::Ingestor::import(scraping, packages)
            .await
            .map_err(|error| error.to_string())?;
        Ok(imported
            .iter()
            .map(|(launchable, job)| {
                json!({
                    "launchable": Self::launchable_value(launchable), "scrapeJob": job,
                })
            })
            .collect())
    }
}
