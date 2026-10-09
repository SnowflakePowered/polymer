use super::*;

#[derive(Clone, Copy)]
pub(super) enum OverlayEntry {
    File,
    Directory,
    Deleted,
}

#[derive(Default)]
pub(super) struct PendingMetadata {
    pub(super) volume: Option<VolumeInfo>,
    pub(super) artifacts: BTreeMap<String, Option<ArtifactRecord>>,
    pub(super) save_tips: BTreeMap<u64, Option<gix::ObjectId>>,
}

#[derive(Default)]
pub(super) struct WorkingState {
    pub(super) entries: BTreeMap<String, OverlayEntry>,
    pub(super) metadata: PendingMetadata,
}

impl PendingMetadata {
    pub(super) fn is_empty(&self) -> bool {
        self.volume.is_none() && self.artifacts.is_empty() && self.save_tips.is_empty()
    }
}

impl WorkingState {
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.metadata.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct PathActivity {
    pub(crate) writing: usize,
    pub(crate) generation: u64,
}

impl PathActivity {
    pub(crate) fn settled(self) -> bool {
        self.writing == 0
    }
}

pub(super) struct GitVolumeInner {
    pub(super) platform: PlatformRepository,
    pub(super) kind: StorageKind,
    pub(super) id: Uuid,
    pub(super) working: RwLock<WorkingState>,
    pub(super) checkout: DirectoryStorage,
    pub(super) index_path: PathBuf,
    pub(super) cache_ready: Mutex<bool>,
    pub(super) changes: Arc<Notify>,
    pub(super) activity: Mutex<BTreeMap<String, PathActivity>>,
    pub(super) passes: std::sync::atomic::AtomicUsize,
}

pub(crate) struct WritePass(Arc<GitVolumeInner>);

impl Drop for WritePass {
    fn drop(&mut self) {
        self.0
            .passes
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        self.0.changes.notify_one();
    }
}

impl GitVolumeInner {
    pub(super) fn touch(&self, path: &str) {
        self.activity
            .lock()
            .entry(path.to_owned())
            .or_default()
            .generation += 1;
    }

    pub(super) fn activity(&self, root: &str) -> PathActivity {
        PendingScope::Within(root)
            .entries(&self.activity.lock())
            .fold(
                PathActivity {
                    writing: self.passes.load(std::sync::atomic::Ordering::SeqCst),
                    generation: 0,
                },
                |total, (_, activity)| PathActivity {
                    writing: total.writing + activity.writing,
                    generation: total.generation + activity.generation,
                },
            )
    }
}

#[derive(Clone)]
pub struct GitVolumeStorage {
    pub(super) inner: Arc<GitVolumeInner>,
    pub(super) artifacts: GitArtifactRegistry,
}

impl fmt::Debug for GitVolumeStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitVolumeStorage")
            .field("platform", &self.inner.platform.platform())
            .field("kind", &self.inner.kind)
            .field("id", &self.inner.id)
            .finish_non_exhaustive()
    }
}

impl GitVolumeStorage {
    pub(super) fn new(platform: PlatformRepository, kind: StorageKind, id: Uuid) -> Self {
        static VOLUMES: std::sync::OnceLock<
            dashmap::DashMap<(PathBuf, StorageKind, Uuid), std::sync::Weak<GitVolumeInner>>,
        > = std::sync::OnceLock::new();
        let git_dir = platform.repository().git_dir();
        let key = (
            fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_owned()),
            kind,
            id,
        );
        let mut entry = VOLUMES
            .get_or_init(dashmap::DashMap::new)
            .entry(key)
            .or_default();
        let inner = entry.upgrade().unwrap_or_else(|| {
            let checkout_root = platform.checkout_path(kind, id);
            let index_path = platform.repository().git_dir().join("index");
            let inner = Arc::new(GitVolumeInner {
                platform,
                kind,
                id,
                working: RwLock::new(WorkingState::default()),
                checkout: DirectoryStorage::unmanaged(checkout_root),
                index_path,
                cache_ready: Mutex::new(false),
                changes: Arc::new(Notify::new()),
                activity: Mutex::new(BTreeMap::new()),
                passes: std::sync::atomic::AtomicUsize::new(0),
            });
            *entry = Arc::downgrade(&inner);
            inner
        });
        drop(entry);
        Self {
            artifacts: GitArtifactRegistry(inner.clone()),
            inner,
        }
    }

    pub fn id(&self) -> Uuid {
        self.inner.id
    }

    pub fn kind(&self) -> StorageKind {
        self.inner.kind
    }

    pub fn platform_repository(&self) -> &PlatformRepository {
        &self.inner.platform
    }

    pub fn checkout_path(&self) -> &Path {
        self.inner.checkout.root()
    }

    pub fn artifact_summary(&self, id: &str) -> Result<Option<ArtifactSummary>, Error> {
        self.recover_components()?;
        let repo = self.inner.platform.repository().to_thread_local();
        let reference = self.ref_name();
        ArtifactRepository::new(&repo, &reference)
            .get(id)?
            .map(|tree| tree.summary())
            .transpose()
    }

    pub fn index_path(&self) -> &Path {
        &self.inner.index_path
    }

    pub(crate) fn ref_name(&self) -> String {
        self.inner.kind.ref_name(self.inner.id)
    }

    pub(super) fn validate(path: &str) -> Result<(), Error> {
        if path.is_empty() {
            return Ok(());
        }
        if path.split('/').any(|component| {
            component.is_empty()
                || component == "."
                || component == ".."
                || component == ".git"
                || component.contains('\\')
        }) {
            return Err(Error::InvalidPath(path.to_owned()));
        }
        Ok(())
    }
}

impl GitVolumeStorage {
    pub(super) fn read_value(&self, path: &str) -> Result<Option<Vec<u8>>, Error> {
        let repo = self.inner.platform.repository().to_thread_local();
        let tip = Self::tip_in(&repo, &self.ref_name())?;
        let tree = Self::root_tree(&repo, tip)?;
        let Some(entry) = tree
            .lookup_entry_by_path(path)
            .map_err(|error| Error::git("lookup metadata value", error))?
        else {
            return Ok(None);
        };
        if entry.mode().kind() != gix::objs::tree::EntryKind::Blob {
            return Err(Error::HistoryConflict(format!(
                "metadata `{path}` is not a blob"
            )));
        }
        repo.find_blob(entry.oid())
            .map(|blob| Some(blob.data.clone()))
            .map_err(|error| Error::git("read metadata value", error))
    }

    pub(crate) fn hold_writes(&self) -> WritePass {
        self.inner
            .passes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        WritePass(self.inner.clone())
    }

    pub(crate) fn activity(&self, root: &str) -> PathActivity {
        self.inner.activity(root)
    }

    pub(crate) fn pending_within(&self, root: &str) -> Vec<crate::ChangedEntry> {
        PendingScope::Within(root)
            .entries(&self.inner.working.read().entries)
            .map(|(path, entry)| crate::ChangedEntry {
                path: path.clone(),
                kind: match entry {
                    OverlayEntry::File => crate::ChangeKind::File,
                    OverlayEntry::Directory => crate::ChangeKind::Directory,
                    OverlayEntry::Deleted => crate::ChangeKind::Removed,
                },
            })
            .collect()
    }

    pub(crate) fn set_save_tip(&self, depot: u64, tip: Option<gix::ObjectId>) {
        self.inner
            .working
            .write()
            .metadata
            .save_tips
            .insert(depot, tip);
    }

    pub(super) fn volume_info_sync(&self) -> Result<Option<VolumeInfo>, Error> {
        let Some(id) = self.read_value("meta/volume/id")? else {
            return Ok(None);
        };
        let text = |value: Option<Vec<u8>>, name: &str| -> Result<String, Error> {
            String::from_utf8(value.ok_or_else(|| {
                Error::HistoryConflict(format!("volume metadata has no `{name}`"))
            })?)
            .map_err(|error| Error::git("decode volume metadata", error))
        };
        let id = text(Some(id), "id")?
            .parse::<Uuid>()
            .map_err(|error| Error::git("parse volume id", error))?;
        let platform = text(self.read_value("meta/volume/platform")?, "platform")?;
        let created = text(self.read_value("meta/volume/created")?, "created")?
            .parse::<i64>()
            .map_err(|error| Error::git("parse volume creation time", error))?;
        let label = self
            .read_value("meta/volume/label")?
            .map(String::from_utf8)
            .transpose()
            .map_err(|error| Error::git("decode volume label", error))?;
        let kind = text(self.read_value("meta/volume/kind")?, "kind")?;
        if id != self.inner.id
            || platform != self.inner.platform.platform()
            || kind != self.inner.kind.metadata_name()
        {
            return Err(Error::HistoryConflict(format!(
                "volume metadata does not match {} {} {}",
                self.inner.platform.platform(),
                self.inner.kind.metadata_name(),
                self.inner.id
            )));
        }
        Ok(Some(VolumeInfo {
            id,
            platform,
            label,
            created: Timestamp::from_second(created)?,
        }))
    }
}
