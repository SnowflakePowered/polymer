pub(crate) mod api;
mod server;

use std::{rc::Rc, sync::Arc};

use actix::{Actor, Addr, Context, System};
use polymer_deno::polymer::library::Library;
use polymer_library::{Directories, GameCatalog, jobs::JobStore};

#[derive(Clone)]
pub struct LibraryService {
    configuration_locks: crate::launchable_configuration::ConfigurationLocks,
    catalog: Arc<GameCatalog>,
    actor: Addr<LibraryBackend>,
    reader: Addr<LibraryReader>,
    scraping: polymer_scraping::ScrapingEngine,
}

// Both mailboxes share one system, which lives until both actors have stopped.
struct LibraryActorLifetime;

impl Drop for LibraryActorLifetime {
    fn drop(&mut self) {
        System::current().stop();
    }
}

struct LibraryReader {
    catalog: Arc<GameCatalog>,
    scraping: polymer_scraping::ScrapingEngine,
    media: Arc<server::MediaServer>,
    _lifetime: Rc<LibraryActorLifetime>,
}

impl Actor for LibraryReader {
    type Context = Context<Self>;
}

struct LibraryBackend {
    firmware_root: std::path::PathBuf,
    key_root: std::path::PathBuf,
    catalog: Arc<GameCatalog>,
    scraping: polymer_scraping::ScrapingEngine,
    _lifetime: Rc<LibraryActorLifetime>,
}

impl Actor for LibraryBackend {
    type Context = Context<Self>;
}

impl LibraryService {
    pub async fn open_with_plugins(
        directories: &Directories,
        plugins: polymer_pluginhost::loading::PluginRegistry,
        jobs: Arc<JobStore>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let catalog = Arc::new(GameCatalog::open_local(directories).await?);
        Ok(Self::start(catalog, plugins, jobs, directories.clone()).await?)
    }

    async fn start(
        catalog: Arc<GameCatalog>,
        plugins: polymer_pluginhost::loading::PluginRegistry,
        jobs: Arc<JobStore>,
        directories: Directories,
    ) -> Result<Self, std::io::Error> {
        let media = server::MediaServer::start(catalog.clone()).await?;
        let firmware_root = directories.firmware();
        let key_root = directories.keys();
        let scraping =
            polymer_scraping::ScrapingEngine::new(catalog.clone(), jobs, plugins, directories);
        if let Err(error) = scraping.resume().await {
            tracing::warn!(%error, "could not resume scrape jobs");
        }
        let actor_catalog = catalog.clone();
        let actor_scraping = scraping.clone();
        let (ready, startup) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("polymer-library".to_owned())
            .spawn(move || {
                let system = System::new();
                let actor = system.block_on(async move {
                    let lifetime = Rc::new(LibraryActorLifetime);
                    let reader = LibraryReader {
                        catalog: actor_catalog.clone(),
                        scraping: actor_scraping.clone(),
                        media: Arc::new(media),
                        _lifetime: lifetime.clone(),
                    }
                    .start();
                    let writer = LibraryBackend::create(move |ctx| {
                        ctx.set_mailbox_capacity(0);
                        LibraryBackend {
                            firmware_root,
                            key_root,
                            catalog: actor_catalog,
                            scraping: actor_scraping,
                            _lifetime: lifetime,
                        }
                    });
                    (writer, reader)
                });
                if ready.send(actor).is_ok()
                    && let Err(error) = system.run()
                {
                    tracing::error!(%error, "library actor system failed");
                }
            })?;
        let (actor, reader) = startup.await.map_err(std::io::Error::other)?;

        Ok(Self {
            configuration_locks: Default::default(),
            actor,
            reader,
            scraping,
            catalog,
        })
    }

    pub fn client(&self) -> Library {
        Library {
            import: self.actor.clone().recipient(),
            import_key: self.actor.clone().recipient(),
            keys_list: self.actor.clone().recipient(),
            keys_platforms: self.actor.clone().recipient(),
            keys_remove: self.actor.clone().recipient(),
            games_list: self.reader.clone().recipient(),
            games_get: self.reader.clone().recipient(),
            games_delete: self.actor.clone().recipient(),
            launchables_add: self.actor.clone().recipient(),
            launchables_find: self.reader.clone().recipient(),
            launchables_delete: self.actor.clone().recipient(),
            metadata_add: self.actor.clone().recipient(),
            metadata_update: self.actor.clone().recipient(),
            metadata_delete: self.actor.clone().recipient(),
            metadata_replace: self.actor.clone().recipient(),
            metadata_text: self.reader.clone().recipient(),
            metadata_url: self.reader.clone().recipient(),
            scraping_start: self.actor.clone().recipient(),
            scraping_scrapers: self.reader.clone().recipient(),
            scraping_refresh: self.actor.clone().recipient(),
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

    pub(crate) fn scraping(&self) -> polymer_scraping::ScrapingEngine {
        self.scraping.clone()
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<polymer_scraping::ScrapingEvent> {
        self.scraping.subscribe()
    }
}
