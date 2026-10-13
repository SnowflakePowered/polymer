mod discover;
mod reconcile;
mod watch;

use actix::{
    Actor, ActorFutureExt, AsyncContext, Context, Handler, Message, ResponseActFuture, WrapFuture,
};
use discover::Discovery;
use polymer_deno::polymer::library::{
    LibraryError, SourcesAdd, SourcesList, SourcesRemove, SourcesScan,
};
use polymer_library::{
    GameCatalog, LibrarySource,
    jobs::{JobKind, JobProgress, JobSpec, JobState, JobStore},
};
use polymer_scraping::ScrapingEngine;
pub(crate) use reconcile::PathLocks;
use reconcile::Reconciler;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use uuid::Uuid;
use watch::SourceWatch;

/// Watches library sources, scans them when they appear, and keeps the
/// catalog in line with what they contain.
pub(crate) struct SourceActor {
    catalog: Arc<GameCatalog>,
    scraping: ScrapingEngine,
    jobs: Arc<JobStore>,
    reconciler: Reconciler,
    events: broadcast::Sender<Value>,
    sources: Vec<LibrarySource>,
    watches: HashMap<Uuid, SourceWatch>,
    offline: HashSet<Uuid>,
    unwatchable: HashSet<Uuid>,
    scans: HashSet<Uuid>,
}

#[derive(Message)]
#[rtype(result = "()")]
pub(super) struct SourceChanged {
    pub id: Uuid,
    pub rescan: bool,
    pub paths: Vec<PathBuf>,
}

#[derive(Message)]
#[rtype(result = "()")]
struct ScanFinished(Uuid);

impl Actor for SourceActor {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Context<Self>) {
        self.refresh(ctx);
        ctx.run_interval(Self::ROOT_CHECK, |actor, ctx| actor.refresh(ctx));
        ctx.run_interval(Self::REARM, |actor, ctx| {
            actor.watches.clear();
            actor.refresh(ctx);
        });
    }
}

impl SourceActor {
    /// How often unreachable roots are checked for, since nothing reports a drive returning.
    const ROOT_CHECK: Duration = Duration::from_secs(10);
    /// Windows drops watches and overflowed changes without reporting either,
    /// so every watch is periodically rebuilt and its source rescanned.
    const REARM: Duration = Duration::from_secs(30 * 60);
    const SCAN_POLL: Duration = Duration::from_millis(500);

    pub(crate) fn new(
        catalog: Arc<GameCatalog>,
        scraping: ScrapingEngine,
        jobs: Arc<JobStore>,
        key_root: PathBuf,
        locks: PathLocks,
        events: broadcast::Sender<Value>,
    ) -> Self {
        Self {
            reconciler: Reconciler::start(catalog.clone(), scraping.clone(), key_root, locks),
            catalog,
            scraping,
            jobs,
            events,
            sources: Vec::new(),
            watches: HashMap::new(),
            offline: HashSet::new(),
            unwatchable: HashSet::new(),
            scans: HashSet::new(),
        }
    }

    /// Re-reads the sources, then watches and scans each root that became
    /// reachable and drops the watch of each that became unreachable.
    fn refresh(&mut self, ctx: &mut Context<Self>) {
        let catalog = self.catalog.clone();
        ctx.spawn(
            async move {
                let sources = catalog.sources().await.map_err(|error| error.to_string())?;
                let roots: Vec<_> = sources
                    .iter()
                    .map(|source| (source.id, source.root.clone()))
                    .collect();
                let reachable = tokio::task::spawn_blocking(move || {
                    roots
                        .into_iter()
                        .filter(|(_, root)| root.is_dir())
                        .map(|(id, _)| id)
                        .collect::<HashSet<_>>()
                })
                .await
                .map_err(|error| error.to_string())?;
                Ok::<_, String>((sources, reachable))
            }
            .into_actor(self)
            .map(|refreshed, actor, ctx| match refreshed {
                Ok((sources, reachable)) => {
                    actor.sources = sources;
                    actor.arm(&reachable, ctx);
                }
                Err(error) => tracing::warn!(%error, "could not read library sources"),
            }),
        );
    }

    fn arm(&mut self, reachable: &HashSet<Uuid>, ctx: &mut Context<Self>) {
        let mut changed = false;
        let known: HashSet<_> = self.sources.iter().map(|source| source.id).collect();
        self.watches.retain(|id, _| known.contains(id));
        self.offline.retain(|id| known.contains(id));
        for source in self.sources.clone() {
            if !reachable.contains(&source.id) {
                changed |= self.watches.remove(&source.id).is_some();
                changed |= self.offline.insert(source.id);
                continue;
            }
            if self.watches.contains_key(&source.id) {
                continue;
            }
            match SourceWatch::start(source.id, &source.root, ctx.address()) {
                Ok(watch) => {
                    self.watches.insert(source.id, watch);
                    self.offline.remove(&source.id);
                    self.unwatchable.remove(&source.id);
                    self.scan(&source, ctx);
                    changed = true;
                }
                Err(error) => {
                    if self.unwatchable.insert(source.id) {
                        tracing::warn!(%error, root = %source.root.display(), "could not watch library source");
                    }
                }
            }
        }
        if changed {
            self.publish();
        }
    }

    /// Walks a source and reconciles every unit on disk and every row the
    /// catalog holds under it, as a `scan` job.
    fn scan(&mut self, source: &LibrarySource, ctx: &mut Context<Self>) {
        if !self.scans.insert(source.id) {
            return;
        }
        let spec = JobSpec {
            kind: JobKind::Scan,
            parent: None,
            title: source.root.display().to_string(),
            subject: Default::default(),
            params: json!({ "source": source.id.simple().to_string() }),
            dedupe: Some(source.id.simple().to_string()),
            steps: Vec::new(),
            persistent: false,
        };
        let (jobs, catalog, reconciler) = (
            self.jobs.clone(),
            self.catalog.clone(),
            self.reconciler.clone(),
        );
        let (id, root, actor) = (source.id, source.root.clone(), ctx.address());
        actix::spawn(async move {
            let scanned = async {
                let job = jobs.start(spec).await.map_err(|error| error.to_string())?;
                let walk = root.clone();
                let mut units = tokio::task::spawn_blocking(move || Discovery::units(&walk))
                    .await
                    .map_err(|error| error.to_string())?;
                units.extend(
                    catalog
                        .source_paths(&root)
                        .await
                        .map_err(|error| error.to_string())?,
                );
                for unit in &units {
                    reconciler.request(unit.clone());
                }
                let total = units.len() as u64;
                loop {
                    let pending = reconciler.pending(&units) as u64;
                    let _ = job
                        .progress(JobProgress {
                            completed: Some(total - pending),
                            total: Some(total),
                            ..Default::default()
                        })
                        .await;
                    if pending == 0 {
                        break;
                    }
                    tokio::time::sleep(Self::SCAN_POLL).await;
                }
                job.finish(JobState::Completed, None)
                    .await
                    .map_err(|error| error.to_string())
            };
            if let Err(error) = scanned.await {
                tracing::warn!(%error, root = %root.display(), "library source scan failed");
            }
            actor.do_send(ScanFinished(id));
        });
        self.publish();
    }

    fn status(&self, source: &LibrarySource) -> Value {
        let state = if self.offline.contains(&source.id) {
            "offline"
        } else if self.scans.contains(&source.id) {
            "scanning"
        } else if self.watches.contains_key(&source.id) {
            "watching"
        } else {
            "idle"
        };
        json!({
            "id": source.id.simple().to_string(),
            "root": source.root,
            "createdAt": source.created_at,
            "state": state,
        })
    }

    fn statuses(&self) -> Value {
        Value::Array(
            self.sources
                .iter()
                .map(|source| self.status(source))
                .collect(),
        )
    }

    fn publish(&self) {
        let _ = self.events.send(self.statuses());
    }

    fn parse(id: &str) -> Result<Uuid, LibraryError> {
        Uuid::parse_str(id).map_err(|_| LibraryError::Type(format!("`{id}` is not a source id")))
    }
}

impl Handler<SourceChanged> for SourceActor {
    type Result = ();

    fn handle(&mut self, change: SourceChanged, ctx: &mut Context<Self>) {
        if change.rescan {
            if let Some(source) = self.sources.iter().find(|source| source.id == change.id) {
                self.scan(&source.clone(), ctx);
            }
            return;
        }
        for path in change.paths {
            self.reconciler.request(path);
        }
    }
}

impl Handler<ScanFinished> for SourceActor {
    type Result = ();

    fn handle(&mut self, ScanFinished(id): ScanFinished, _: &mut Context<Self>) {
        self.scans.remove(&id);
        self.publish();
    }
}

impl Handler<SourcesList> for SourceActor {
    type Result = Result<Value, LibraryError>;

    fn handle(&mut self, _: SourcesList, _: &mut Context<Self>) -> Self::Result {
        Ok(self.statuses())
    }
}

impl Handler<SourcesAdd> for SourceActor {
    type Result = ResponseActFuture<Self, Result<Value, LibraryError>>;

    fn handle(&mut self, SourcesAdd { path }: SourcesAdd, _: &mut Context<Self>) -> Self::Result {
        let catalog = self.catalog.clone();
        Box::pin(
            async move {
                let probe = path.clone();
                let root = tokio::task::spawn_blocking(move || {
                    dunce::canonicalize(&probe).map(|root| root.is_dir().then_some(root))
                })
                .await
                .map_err(|error| LibraryError::Generic(error.to_string()))?
                .map_err(|error| LibraryError::Type(format!("{path}: {error}")))?
                .ok_or_else(|| LibraryError::Type(format!("{path} is not a folder")))?;
                catalog
                    .add_source(&root)
                    .await
                    .map_err(|error| LibraryError::Generic(error.to_string()))
            }
            .into_actor(self)
            .map(|added, actor, ctx| {
                let added = added?;
                if !actor.sources.contains(&added) {
                    actor.sources.push(added.clone());
                }
                actor.refresh(ctx);
                Ok(actor.status(&added))
            }),
        )
    }
}

impl Handler<SourcesRemove> for SourceActor {
    type Result = ResponseActFuture<Self, Result<(), LibraryError>>;

    fn handle(
        &mut self,
        SourcesRemove { id }: SourcesRemove,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let parsed = Self::parse(&id);
        let (catalog, scraping) = (self.catalog.clone(), self.scraping.clone());
        Box::pin(
            async move {
                let id = parsed?;
                for root in catalog
                    .remove_source(id)
                    .await
                    .map_err(|error| LibraryError::Generic(error.to_string()))?
                {
                    scraping.publish_library_content(root);
                }
                Ok::<_, LibraryError>(id)
            }
            .into_actor(self)
            .map(|removed, actor, _| {
                let id = removed?;
                actor.watches.remove(&id);
                actor.sources.retain(|source| source.id != id);
                actor.publish();
                Ok(())
            }),
        )
    }
}

impl Handler<SourcesScan> for SourceActor {
    type Result = Result<(), LibraryError>;

    fn handle(&mut self, SourcesScan { id }: SourcesScan, ctx: &mut Context<Self>) -> Self::Result {
        let id = Self::parse(&id)?;
        let source = self
            .sources
            .iter()
            .find(|source| source.id == id)
            .cloned()
            .ok_or_else(|| LibraryError::Type(format!("no library source {id}")))?;
        if self.offline.contains(&id) {
            return Err(LibraryError::Generic(format!(
                "{} is not reachable",
                source.root.display()
            )));
        }
        self.scan(&source, ctx);
        Ok(())
    }
}
