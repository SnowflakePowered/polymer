use super::discover::{DirectoryKind, Discovery};
use dashmap::{DashMap, mapref::entry::Entry};
use polymer_consolestorage::impls::ps3::{Ps3Nand, rap::Rap};
use polymer_consolestorage::installable::{DepotType, InPlaceTitle};
use polymer_consolestorage::keys::{KeyRecord, KeyWriter};
use polymer_library::{
    GameCatalog, GameCatalogError, ImportOrigin, LaunchableUri, LibrarySource, MountedTitle,
};
use polymer_scraping::{Ingestor, Package, ScrapingEngine};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{Mutex, OwnedMutexGuard, Semaphore, mpsc};

/// Per-path locks shared by source reconciles and explicit imports, so an
/// identification is never committed over a newer one for the same path.
#[derive(Clone, Default)]
pub(crate) struct PathLocks(Arc<DashMap<String, Arc<Mutex<()>>>>);

/// Holds a path's lock and forgets the lock once nobody else wants it.
pub(crate) struct PathGuard {
    guard: Option<OwnedMutexGuard<()>>,
    locks: PathLocks,
    key: String,
}

impl PathLocks {
    pub(crate) async fn lock(&self, path: &Path) -> PathGuard {
        let key = LibrarySource::key(path);
        let lock = self.0.entry(key.clone()).or_default().clone();
        PathGuard {
            guard: Some(lock.lock_owned().await),
            locks: self.clone(),
            key,
        }
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        drop(self.guard.take());
        self.locks
            .0
            .remove_if(&self.key, |_, lock| Arc::strong_count(lock) == 1);
    }
}

enum KeyState {
    Queued,
    Running { dirty: bool },
}

/// Brings the catalog in line with the disk one path at a time. A path
/// requested while it is being reconciled is reconciled again afterwards.
#[derive(Clone)]
pub(super) struct Reconciler {
    inner: Arc<Inner>,
}

struct Inner {
    catalog: Arc<GameCatalog>,
    scraping: ScrapingEngine,
    key_root: PathBuf,
    locks: PathLocks,
    states: DashMap<String, KeyState>,
    queue: mpsc::UnboundedSender<PathBuf>,
    /// Stamps of files already identified as not being games.
    ignored: DashMap<String, String>,
    /// In-place titles that lost their install path to the keyed host directory.
    shadowed: DashMap<String, Vec<PathBuf>>,
}

impl Reconciler {
    const WORKERS: usize = 2;
    const SETTLE: Duration = Duration::from_secs(3);

    pub fn start(
        catalog: Arc<GameCatalog>,
        scraping: ScrapingEngine,
        key_root: PathBuf,
        locks: PathLocks,
    ) -> Self {
        let (queue, mut requests) = mpsc::unbounded_channel();
        let reconciler = Self {
            inner: Arc::new(Inner {
                catalog,
                scraping,
                key_root,
                locks,
                states: DashMap::new(),
                queue,
                ignored: DashMap::new(),
                shadowed: DashMap::new(),
            }),
        };
        let dispatcher = reconciler.clone();
        actix::spawn(async move {
            let workers = Arc::new(Semaphore::new(Self::WORKERS));
            while let Some(path) = requests.recv().await {
                let Ok(permit) = workers.clone().acquire_owned().await else {
                    break;
                };
                let reconciler = dispatcher.clone();
                actix::spawn(async move {
                    reconciler.run(path).await;
                    drop(permit);
                });
            }
        });
        reconciler
    }

    pub fn request(&self, path: PathBuf) {
        match self.inner.states.entry(LibrarySource::key(&path)) {
            Entry::Vacant(entry) => {
                entry.insert(KeyState::Queued);
                let _ = self.inner.queue.send(path);
            }
            Entry::Occupied(mut entry) => {
                if let KeyState::Running { dirty } = entry.get_mut() {
                    *dirty = true;
                }
            }
        }
    }

    /// How many of `paths` still have a reconcile queued or running.
    pub fn pending(&self, paths: &[PathBuf]) -> usize {
        paths
            .iter()
            .filter(|path| self.inner.states.contains_key(&LibrarySource::key(path)))
            .count()
    }

    async fn run(&self, path: PathBuf) {
        let key = LibrarySource::key(&path);
        self.inner
            .states
            .insert(key.clone(), KeyState::Running { dirty: false });
        let guard = self.inner.locks.lock(&path).await;
        if let Err(error) = self.reconcile(&path).await {
            tracing::warn!(%error, path = %path.display(), "could not reconcile library source path");
        }
        drop(guard);
        let rerun = match self.inner.states.entry(key) {
            Entry::Occupied(entry) if matches!(entry.get(), KeyState::Running { dirty: true }) => {
                entry.replace_entry(KeyState::Queued);
                true
            }
            Entry::Occupied(entry) => {
                entry.remove();
                false
            }
            Entry::Vacant(_) => false,
        };
        if rerun {
            let _ = self.inner.queue.send(path);
        }
    }

    async fn reconcile(&self, path: &Path) -> Result<(), String> {
        let sources = self
            .inner
            .catalog
            .sources()
            .await
            .map_err(|error| error.to_string())?;
        let Some(source) = sources
            .into_iter()
            .filter(|source| source.covers(path))
            .max_by_key(|source| source.root.as_os_str().len())
        else {
            return self.forget(path, None).await;
        };
        if let Some(directory) = Discovery::marked(path) {
            self.request(directory);
        }
        match tokio::fs::metadata(path).await {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.forget(path, Some(&source.root)).await?;
                self.request_rows(path).await
            }
            Err(error) => Err(error.to_string()),
            Ok(metadata) if metadata.is_dir() => self.directory(&source, path).await,
            Ok(metadata) => self.file(&source, path, metadata.modified().ok()).await,
        }
    }

    async fn directory(&self, source: &LibrarySource, path: &Path) -> Result<(), String> {
        let (probe, root) = (path.to_owned(), source.root.clone());
        let kind = tokio::task::spawn_blocking(move || Discovery::classify(&probe, &root))
            .await
            .map_err(|error| error.to_string())?;
        let outcome = match kind {
            DirectoryKind::Title(title) => self.mount(source, path, title).await,
            DirectoryKind::Disc => {
                let stamp = Discovery::stamp(Discovery::identity(path, None));
                self.import(source, path, &stamp).await
            }
            DirectoryKind::Application => self.forget(path, Some(&source.root)).await,
            DirectoryKind::Folder => self.expand(source, path).await,
        };
        self.request_rows(path).await?;
        outcome
    }

    async fn expand(&self, source: &LibrarySource, path: &Path) -> Result<(), String> {
        self.forget(path, Some(&source.root)).await?;
        let root = path.to_owned();
        let units = tokio::task::spawn_blocking(move || Discovery::units(&root))
            .await
            .map_err(|error| error.to_string())?;
        for unit in units.into_iter().filter(|unit| unit != path) {
            self.request(unit);
        }
        Ok(())
    }

    async fn file(
        &self,
        source: &LibrarySource,
        path: &Path,
        modified: Option<SystemTime>,
    ) -> Result<(), String> {
        let (root, probe) = (source.root.clone(), path.to_owned());
        let unit = tokio::task::spawn_blocking(move || Discovery::unit_of(&root, &probe))
            .await
            .map_err(|error| error.to_string())?;
        match unit {
            Some(unit) if unit == path && Discovery::marked(path).is_none() => {}
            Some(unit) if unit != path => {
                self.request(unit);
                return self.forget(path, Some(&source.root)).await;
            }
            _ => return self.forget(path, Some(&source.root)).await,
        }
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("rap"))
        {
            let record = Rap::read(path).map_err(|error| error.to_string())?;
            return self.keys(Ps3Nand::PLATFORM, vec![record]).await;
        }
        let settling = modified
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|elapsed| elapsed < Self::SETTLE);
        if settling {
            let reconciler = self.clone();
            let path = path.to_owned();
            actix::spawn(async move {
                tokio::time::sleep(Self::SETTLE).await;
                reconciler.request(path);
            });
            return Ok(());
        }
        self.import(source, path, &Discovery::stamp([path.to_owned()]))
            .await
    }

    /// Identifies a file or disc folder and imports it if it is a game, unless
    /// it is unchanged since it was last identified. Installable packages are
    /// never installed from a source.
    async fn import(&self, source: &LibrarySource, path: &Path, stamp: &str) -> Result<(), String> {
        let key = LibrarySource::key(path);
        if self.fresh(path, stamp).await?
            || self
                .inner
                .ignored
                .get(&key)
                .is_some_and(|ignored| *ignored == stamp)
        {
            return Ok(());
        }
        let probe = path.to_owned();
        let packages = tokio::task::spawn_blocking(move || Ingestor::identify(&probe, None, None))
            .await
            .map_err(|error| error.to_string())?;
        match packages {
            Ok(packages)
                if !packages.is_empty() && packages.iter().all(|package| !package.installable) =>
            {
                self.inner.ignored.remove(&key);
                self.commit(packages, path, stamp).await
            }
            _ => {
                self.forget(path, Some(&source.root)).await?;
                if tokio::fs::metadata(&source.root).await.is_ok() {
                    self.inner.ignored.insert(key, stamp.to_owned());
                }
                Ok(())
            }
        }
    }

    async fn fresh(&self, path: &Path, stamp: &str) -> Result<bool, String> {
        self.inner
            .catalog
            .stamped(path, stamp)
            .await
            .map_err(|error| error.to_string())
    }

    async fn mount(
        &self,
        source: &LibrarySource,
        path: &Path,
        title: InPlaceTitle,
    ) -> Result<(), String> {
        match self.inner.catalog.mount_title(path, &title).await {
            Err(GameCatalogError::MountedElsewhere(winner)) => {
                self.inner
                    .shadowed
                    .entry(LibrarySource::key(&winner))
                    .or_default()
                    .push(path.to_owned());
                return self.forget(path, Some(&source.root)).await;
            }
            Err(GameCatalogError::Uncovered(_)) => return Ok(()),
            result => result.map_err(|error| error.to_string())?,
        }
        self.keys(&title.platform, title.keys.clone()).await?;
        let Some(entrypoint) = title.entrypoint.filter(|_| title.kind == DepotType::App) else {
            return Ok(());
        };
        let stamp = Discovery::stamp(Discovery::identity(path, Some(&entrypoint.path)));
        if self.fresh(path, &stamp).await? {
            return Ok(());
        }
        let probe = path.to_owned();
        let platform = title.platform;
        let identify = platform.clone();
        let mut package = tokio::task::spawn_blocking(move || {
            Package::identify(&probe, Some(identify.as_str()), None)
        })
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
        package.uri = LaunchableUri::Mounted(MountedTitle {
            platform,
            install_path: title.install_path,
            host: path.to_owned(),
            entrypoint: entrypoint.path,
        });
        self.commit(vec![package], path, &stamp).await
    }

    async fn commit(&self, packages: Vec<Package>, path: &Path, stamp: &str) -> Result<(), String> {
        match Ingestor::import(&self.inner.scraping, packages, ImportOrigin::Source).await {
            Err(polymer_scraping::ScrapingError::Library(GameCatalogError::Uncovered(_))) => Ok(()),
            Err(error) => Err(error.to_string()),
            Ok(_) => self
                .inner
                .catalog
                .stamp(path, stamp)
                .await
                .map_err(|error| error.to_string()),
        }
    }

    /// Drops what a source imported at `path`. While `root` is given, nothing is
    /// dropped unless it is reachable, so a disappearing drive deletes nothing.
    async fn forget(&self, path: &Path, root: Option<&Path>) -> Result<(), String> {
        if let Some(root) = root
            && tokio::fs::metadata(root).await.is_err()
        {
            return Err(format!("{} is not reachable", root.display()));
        }
        for root in self
            .inner
            .catalog
            .forget(path)
            .await
            .map_err(|error| error.to_string())?
        {
            self.inner.scraping.publish_library_content(root);
        }
        if let Some((_, shadowed)) = self.inner.shadowed.remove(&LibrarySource::key(path)) {
            for path in shadowed {
                self.request(path);
            }
        }
        Ok(())
    }

    /// Reconciles what the catalog holds beneath `path`, which may no longer match the disk.
    async fn request_rows(&self, path: &Path) -> Result<(), String> {
        let rows = self
            .inner
            .catalog
            .source_paths(path)
            .await
            .map_err(|error| error.to_string())?;
        for row in rows.into_iter().filter(|row| row != path) {
            self.request(row);
        }
        Ok(())
    }

    async fn keys(&self, platform: &str, records: Vec<KeyRecord>) -> Result<(), String> {
        if records.is_empty() {
            return Ok(());
        }
        polymer_library::keys::KeyStore::open(&self.inner.key_root)
            .await
            .map_err(|error| error.to_string())?
            .for_platform(platform)
            .map_err(|error| error.to_string())?
            .put(&records)
            .await
            .map_err(|error| error.to_string())
    }
}
