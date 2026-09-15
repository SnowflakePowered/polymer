use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::{BStr, ByteSlice};
use gix::objs::Write as _;
use gix::refs::Target;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use jiff::Timestamp;
use parking_lot::{Mutex, RwLock};
use tokio::io::AsyncRead;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::installable::artifacts::Artifact;
use crate::installable::{DepotFile, DepotType, IconHint};
use crate::storage::manifest::{ArtifactRegistry, ArtifactSummary, VolumeInfo};
use crate::{
    BoxFuture, ChangeSignal, DirectoryStorage, EntryKind, Error, InstallResult, Metadata,
    MountableStorage, ReadDir, Storage, StorageFile, StorageKind, VolumeStorage,
};

const SCHEMA_VERSION: u32 = 2;

impl StorageKind {
    fn ref_name(self, id: Uuid) -> String {
        let namespace = match self {
            Self::Nand => "nands",
            Self::SaveRepo | Self::MemoryCard | Self::Battery => "profiles",
        };
        format!(
            "refs/consoles/{namespace}/v{SCHEMA_VERSION}/{}",
            id.simple()
        )
    }

    fn metadata_name(self) -> &'static str {
        match self {
            Self::Nand => "nand",
            Self::SaveRepo => "save-repo",
            Self::MemoryCard => "memory-card",
            Self::Battery => "battery",
        }
    }

    fn checkout_namespace(self) -> &'static str {
        match self {
            Self::Nand => "nands",
            Self::SaveRepo => "save-repos",
            Self::MemoryCard => "memory-cards",
            Self::Battery => "battery-profiles",
        }
    }
}

struct PlatformInner {
    path: PathBuf,
    platform: String,
    repository: gix::ThreadSafeRepository,
    operation: Mutex<()>,
}

#[derive(Clone)]
pub struct PlatformRepository(Arc<PlatformInner>);

impl fmt::Debug for PlatformRepository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlatformRepository")
            .field("path", &self.0.path)
            .field("platform", &self.0.platform)
            .finish_non_exhaustive()
    }
}

impl PlatformRepository {
    pub fn open(path: impl Into<PathBuf>, platform: &str) -> Result<Self, Error> {
        let path = path.into();
        let repository = if path.join(".git").is_dir() {
            gix::open(&path).map_err(|error| Error::git("open platform repository", error))?
        } else {
            let parent = path
                .parent()
                .ok_or_else(|| Error::InvalidPath(path.display().to_string()))?;
            std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
            gix::ThreadSafeRepository::init(
                &path,
                gix::create::Kind::WithWorktree,
                gix::create::Options::default(),
            )
            .map_err(|error| Error::git("initialize platform repository", error))?
            .to_thread_local()
        };
        let repository = repository.into_sync();
        Self::configure(&repository, platform)?;
        Ok(Self(Arc::new(PlatformInner {
            path,
            platform: platform.to_owned(),
            repository,
            operation: Mutex::new(()),
        })))
    }

    fn configure(repository: &gix::ThreadSafeRepository, platform: &str) -> Result<(), Error> {
        let config_path = repository.git_dir().join("config");
        let mut config = std::fs::read_to_string(&config_path)
            .map_err(|error| Error::io(&config_path, error))?;
        let marker = "# polymer platform storage v2";
        let mut changed = false;
        if !config.contains(marker) {
            config.push_str(&format!(
                "\n{marker}\n[polymer]\n\tschemaVersion = {SCHEMA_VERSION}\n\tplatform = {platform}\n[core]\n\tlogAllRefUpdates = always\n"
            ));
            changed = true;
        }
        let archive_policy = "# consoles recently-deleted retention v2";
        if !config.contains(archive_policy) {
            config.push_str(&format!(
                "\n{archive_policy}\n[gc \"refs/consoles/archive/v{SCHEMA_VERSION}/*/recently-deleted\"]\n\treflogExpireUnreachable = 30 days\n"
            ));
            changed = true;
        }
        if changed {
            std::fs::write(&config_path, &config)
                .map_err(|error| Error::io(&config_path, error))?;
        }
        if !config.contains(&format!("schemaVersion = {SCHEMA_VERSION}"))
            || !config.contains(&format!("platform = {platform}"))
        {
            return Err(Error::HistoryConflict(format!(
                "{} is not a schema {SCHEMA_VERSION} {platform} platform repository",
                repository.git_dir().display()
            )));
        }
        Ok(())
    }

    pub fn platform(&self) -> &str {
        &self.0.platform
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }

    pub fn checkout_root(&self) -> PathBuf {
        self.0.path.join("files")
    }

    fn checkout_path(&self, kind: StorageKind, id: Uuid) -> PathBuf {
        self.checkout_root()
            .join(kind.checkout_namespace())
            .join(id.simple().to_string())
    }

    pub fn volume(&self, kind: StorageKind, id: Uuid) -> GitVolumeStorage {
        GitVolumeStorage::new(self.clone(), kind, id)
    }

    pub fn contains(&self, kind: StorageKind, id: Uuid) -> Result<bool, Error> {
        let repo = self.0.repository.to_thread_local();
        repo.try_find_reference(&kind.ref_name(id))
            .map(|reference| reference.is_some())
            .map_err(|error| Error::git("find volume reference", error))
    }

    pub fn ids(&self, kind: StorageKind) -> Result<Vec<Uuid>, Error> {
        let repo = self.0.repository.to_thread_local();
        let namespace = match kind {
            StorageKind::Nand => "nands",
            StorageKind::SaveRepo | StorageKind::MemoryCard | StorageKind::Battery => "profiles",
        };
        let prefix = format!("refs/consoles/{namespace}/v{SCHEMA_VERSION}/");
        let mut ids = Vec::new();
        let references = repo
            .references()
            .map_err(|error| Error::git("list platform references", error))?;
        for reference in references
            .all()
            .map_err(|error| Error::git("list platform references", error))?
        {
            let reference =
                reference.map_err(|error| Error::git("read platform reference", error))?;
            let name = reference.name().as_bstr().to_str_lossy();
            if let Some(id) = name
                .strip_prefix(&prefix)
                .and_then(|id| Uuid::parse_str(id).ok())
            {
                let volume = self.volume(kind, id);
                if volume
                    .read_value("meta/volume/kind")?
                    .is_some_and(|value| value == kind.metadata_name().as_bytes())
                {
                    ids.push(id);
                }
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }

    pub(crate) fn repository(&self) -> &gix::ThreadSafeRepository {
        &self.0.repository
    }

    pub fn delete(&self, kind: StorageKind, id: Uuid) -> Result<(), Error> {
        let _guard = self.0.operation.lock();
        let repo = self.0.repository.to_thread_local();
        let name = kind.ref_name(id);
        let Some(reference) = repo
            .try_find_reference(&name)
            .map_err(|error| Error::git("find volume reference", error))?
        else {
            return Ok(());
        };
        let mut edits = vec![RefEdit {
            change: Change::Delete {
                expected: PreviousValue::MustExistAndMatch(reference.target().to_owned().into()),
                log: RefLog::AndReference,
            },
            name: gix::refs::FullName::try_from(name.as_str())
                .map_err(|error| Error::git("validate volume reference", error))?,
            deref: false,
        }];
        if kind != StorageKind::Nand {
            let profile = id.simple().to_string();
            let prefixes = [
                format!("refs/consoles/saves/v2/{profile}/"),
                format!("refs/notes/consoles/v2/{profile}/"),
            ];
            let exact = [
                format!("refs/consoles/archive/v2/{profile}/recently-deleted"),
                format!("refs/consoles/transactions/v2/{profile}"),
            ];
            let references = repo
                .references()
                .map_err(|error| Error::git("list profile references", error))?;
            for reference in references
                .all()
                .map_err(|error| Error::git("list profile references", error))?
            {
                let reference =
                    reference.map_err(|error| Error::git("read profile reference", error))?;
                let reference_name = reference.name().as_bstr().to_str_lossy().into_owned();
                if !prefixes
                    .iter()
                    .any(|prefix| reference_name.starts_with(prefix))
                    && !exact.contains(&reference_name)
                {
                    continue;
                }
                edits.push(RefEdit {
                    change: Change::Delete {
                        expected: PreviousValue::MustExistAndMatch(
                            reference.target().to_owned().into(),
                        ),
                        log: RefLog::AndReference,
                    },
                    name: gix::refs::FullName::try_from(reference_name.as_str())
                        .map_err(|error| Error::git("validate profile reference", error))?,
                    deref: false,
                });
            }
        }
        let signature = signature();
        let mut time = gix::date::parse::TimeBuf::default();
        repo.edit_references_as(edits, Some(signature.to_ref(&mut time)))
            .map_err(|error| Error::git("delete volume reference", error))?;
        self.volume(kind, id)
            .remove_from_index(repo.object_hash())?;
        let checkout = self.checkout_path(kind, id);
        if checkout.exists() {
            fs::remove_dir_all(&checkout).map_err(|error| Error::io(&checkout, error))?;
        }
        Ok(())
    }

    pub fn fork_profile(
        &self,
        kind: StorageKind,
        source: Uuid,
        destination: Uuid,
        label: Option<&str>,
    ) -> Result<GitVolumeStorage, Error> {
        if kind == StorageKind::Nand {
            return Err(Error::Unsupported(
                "only save repositories, memory cards, and battery profiles can be forked"
                    .to_owned(),
            ));
        }
        let _guard = self.0.operation.lock();
        let repo = self.0.repository.to_thread_local();
        let source_ref = kind.ref_name(source);
        let destination_ref = kind.ref_name(destination);
        let source_tip = GitVolumeStorage::tip_in(&repo, &source_ref)?
            .ok_or_else(|| Error::not_found(&source_ref))?;
        if GitVolumeStorage::tip_in(&repo, &destination_ref)?.is_some() {
            return Err(Error::HistoryConflict(format!(
                "profile {destination} already exists"
            )));
        }
        let root = GitVolumeStorage::root_tree(&repo, Some(source_tip))?;
        let mut editor = root
            .edit()
            .map_err(|error| Error::git("edit forked profile tree", error))?;
        write_text(
            &repo,
            &mut editor,
            "meta/volume/id",
            &destination.to_string(),
        )?;
        write_text(
            &repo,
            &mut editor,
            "meta/volume/created",
            &Timestamp::now().as_second().to_string(),
        )?;
        match label {
            Some(label) => write_text(&repo, &mut editor, "meta/volume/label", label)?,
            None => {
                editor
                    .remove_leaf("meta/volume/label")
                    .map_err(|error| Error::git("remove forked profile label", error))?;
            }
        }
        let tree = editor
            .write()
            .map_err(|error| Error::git("write forked profile tree", error))?
            .detach();
        let signature = signature();
        let mut time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut time);
        let commit = repo
            .new_commit_as(
                signature,
                signature,
                "Polymer profile fork",
                tree,
                [source_tip],
            )
            .map_err(|error| Error::git("write forked profile commit", error))?
            .id;
        let mut edits = vec![RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: true,
                    message: "polymer: fork profile".into(),
                },
                expected: PreviousValue::MustNotExist,
                new: Target::Object(commit),
            },
            name: gix::refs::FullName::try_from(destination_ref.as_str())
                .map_err(|error| Error::git("validate forked profile reference", error))?,
            deref: false,
        }];
        let source_id = source.simple().to_string();
        let destination_id = destination.simple().to_string();
        let references = repo
            .references()
            .map_err(|error| Error::git("list profile resource references", error))?;
        for reference in references
            .all()
            .map_err(|error| Error::git("list profile resource references", error))?
        {
            let reference =
                reference.map_err(|error| Error::git("read profile resource reference", error))?;
            let name = reference.name().as_bstr().to_str_lossy().into_owned();
            let is_resource = name.starts_with(&format!("refs/consoles/saves/v2/{source_id}/"))
                || name.starts_with(&format!("refs/notes/consoles/v2/{source_id}/"));
            if !is_resource {
                continue;
            }
            let Some(id) = reference.try_id().map(|id| id.detach()) else {
                continue;
            };
            let name = name.replacen(&source_id, &destination_id, 1);
            edits.push(RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: true,
                        message: "polymer: fork profile resource".into(),
                    },
                    expected: PreviousValue::MustNotExist,
                    new: Target::Object(id),
                },
                name: gix::refs::FullName::try_from(name.as_str())
                    .map_err(|error| Error::git("validate forked resource reference", error))?,
                deref: false,
            });
        }
        repo.edit_references_as(edits, Some(signature))
            .map_err(|error| Error::git("publish forked profile", error))?;
        Ok(self.volume(kind, destination))
    }
}

#[derive(Clone, Copy)]
enum OverlayEntry {
    File,
    Directory,
    Deleted,
}

#[derive(Default)]
struct PendingMetadata {
    volume: Option<VolumeInfo>,
    artifacts: BTreeMap<String, Option<ArtifactRecord>>,
    save_tips: BTreeMap<u64, Option<gix::ObjectId>>,
}

#[derive(Default)]
struct WorkingState {
    entries: BTreeMap<String, OverlayEntry>,
    metadata: PendingMetadata,
}

impl WorkingState {
    fn is_empty(&self) -> bool {
        self.entries.is_empty()
            && self.metadata.volume.is_none()
            && self.metadata.artifacts.is_empty()
            && self.metadata.save_tips.is_empty()
    }
}

struct GitVolumeInner {
    platform: PlatformRepository,
    kind: StorageKind,
    id: Uuid,
    working: RwLock<WorkingState>,
    checkout: DirectoryStorage,
    index_path: PathBuf,
    cache_ready: Mutex<bool>,
    changes: Arc<Notify>,
}

#[derive(Clone)]
pub struct GitVolumeStorage {
    inner: Arc<GitVolumeInner>,
    artifacts: GitArtifactRegistry,
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
    fn new(platform: PlatformRepository, kind: StorageKind, id: Uuid) -> Self {
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
        });
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

    pub fn index_path(&self) -> &Path {
        &self.inner.index_path
    }

    pub(crate) fn ref_name(&self) -> String {
        self.inner.kind.ref_name(self.inner.id)
    }

    fn validate(path: &str) -> Result<(), Error> {
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

    fn files_tree_id(repo: &gix::Repository, root: &gix::Tree<'_>) -> Result<gix::ObjectId, Error> {
        match root
            .lookup_entry_by_path("files")
            .map_err(|error| Error::git("lookup checked-out files tree", error))?
        {
            Some(entry) if entry.mode().kind() == gix::objs::tree::EntryKind::Tree => {
                Ok(entry.object_id())
            }
            Some(_) => Err(Error::HistoryConflict(
                "volume files root is not a tree".to_owned(),
            )),
            None => Ok(repo.empty_tree().id),
        }
    }

    fn target_index(
        &self,
        repo: &gix::Repository,
        root: &gix::Tree<'_>,
    ) -> Result<gix::index::File, Error> {
        let tree = Self::files_tree_id(repo, root)?;
        repo.index_from_tree(&tree)
            .map_err(|error| Error::git("build volume checkout index", error))
    }

    fn index_prefix(&self) -> String {
        format!(
            "files/{}/{}/",
            self.inner.kind.checkout_namespace(),
            self.inner.id.simple()
        )
    }

    fn volume_index_matches(&self, platform: &gix::index::File, target: &gix::index::File) -> bool {
        let prefix = self.index_prefix();
        let current = platform
            .entries()
            .iter()
            .filter_map(|entry| {
                entry
                    .path(platform)
                    .to_str_lossy()
                    .strip_prefix(&prefix)
                    .map(|path| (path.to_owned(), entry))
            })
            .collect::<Vec<_>>();
        current.len() == target.entries().len()
            && current
                .iter()
                .zip(target.entries())
                .all(|((path, current), wanted)| {
                    path.as_bytes() == wanted.path(target).as_bytes()
                        && current.id == wanted.id
                        && current.mode == wanted.mode
                })
    }

    fn checkout_is_complete(&self, index: &gix::index::File) -> bool {
        self.inner.checkout.root().is_dir()
            && index.entries().iter().all(|entry| {
                let path = entry.path(index).to_str_lossy();
                Self::validate(&path).is_ok()
                    && self
                        .inner
                        .checkout
                        .root()
                        .join(path.replace('/', std::path::MAIN_SEPARATOR_STR))
                        .is_file()
            })
    }

    fn write_index(&self, target: gix::index::File) -> Result<(), Error> {
        let parent = self
            .inner
            .index_path
            .parent()
            .ok_or_else(|| Error::InvalidPath(self.inner.index_path.display().to_string()))?;
        fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
        let mut index = if self.inner.index_path.is_file() {
            gix::index::File::at(
                &self.inner.index_path,
                target.object_hash(),
                false,
                gix::index::decode::Options::default(),
            )
            .map_err(|error| Error::git("read platform checkout index", error))?
        } else {
            gix::index::File::from_state(
                gix::index::State::new(target.object_hash()),
                &self.inner.index_path,
            )
        };
        let prefix = self.index_prefix();
        index.remove_entries(|_, path, _| path.to_str_lossy().starts_with(&prefix));
        for entry in target.entries() {
            let relative = entry.path(&target).to_str_lossy();
            Self::validate(&relative)?;
            let path = self
                .inner
                .checkout
                .root()
                .join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
            let metadata = gix::index::fs::Metadata::from_path_no_follow(&path)
                .map_err(|error| Error::io(&path, error))?;
            let stat = gix::index::entry::Stat::from_fs(&metadata)
                .map_err(|error| Error::git("record checkout file metadata", error))?;
            let path = format!("{prefix}{relative}");
            index.dangerously_push_entry(
                stat,
                entry.id,
                entry.flags,
                entry.mode,
                BStr::new(path.as_bytes()),
            );
        }
        index.remove_tree();
        index.sort_entries();
        index.set_path(&self.inner.index_path);
        index
            .write(gix::index::write::Options::default())
            .map_err(|error| Error::git("write volume checkout index", error))
    }

    fn remove_from_index(&self, object_hash: gix::hash::Kind) -> Result<(), Error> {
        if !self.inner.index_path.is_file() {
            return Ok(());
        }
        let mut index = gix::index::File::at(
            &self.inner.index_path,
            object_hash,
            false,
            gix::index::decode::Options::default(),
        )
        .map_err(|error| Error::git("read platform checkout index", error))?;
        let prefix = self.index_prefix();
        index.remove_entries(|_, path, _| path.to_str_lossy().starts_with(&prefix));
        index.remove_tree();
        index.set_path(&self.inner.index_path);
        index
            .write(gix::index::write::Options::default())
            .map_err(|error| Error::git("remove volume from checkout index", error))
    }

    fn checkout_target(
        &self,
        repo: &gix::Repository,
        root: &gix::Tree<'_>,
        index: &gix::index::File,
    ) -> Result<(), Error> {
        let checkout = self.inner.checkout.root();
        if checkout.exists() {
            fs::remove_dir_all(checkout).map_err(|error| Error::io(checkout, error))?;
        }
        fs::create_dir_all(checkout).map_err(|error| Error::io(checkout, error))?;

        for entry in index.entries() {
            let relative = entry.path(index).to_str_lossy();
            Self::validate(&relative)?;
            let destination = checkout.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
            let parent = destination
                .parent()
                .ok_or_else(|| Error::InvalidPath(destination.display().to_string()))?;
            fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
            let blob = repo
                .find_blob(entry.id)
                .map_err(|error| Error::git("read checkout blob", error))?;
            let mut file =
                fs::File::create(&destination).map_err(|error| Error::io(&destination, error))?;
            file.write_all(&blob.data)
                .map_err(|error| Error::io(&destination, error))?;
        }

        for directory in self.base_directories(repo, root)? {
            Self::validate(&directory)?;
            let destination = checkout.join(directory.replace('/', std::path::MAIN_SEPARATOR_STR));
            fs::create_dir_all(&destination).map_err(|error| Error::io(&destination, error))?;
        }
        Ok(())
    }

    fn refresh_checkout_locked(
        &self,
        repo: &gix::Repository,
        tip: Option<gix::ObjectId>,
    ) -> Result<(), Error> {
        let root = Self::root_tree(repo, tip)?;
        let index = self.target_index(repo, &root)?;
        self.checkout_target(repo, &root, &index)?;
        self.write_index(index)?;
        *self.inner.cache_ready.lock() = true;
        Ok(())
    }

    fn committed_checkout_entries(
        &self,
        repo: &gix::Repository,
        root: &gix::Tree<'_>,
    ) -> Result<BTreeMap<PathBuf, Option<gix::ObjectId>>, Error> {
        let index = self.target_index(repo, root)?;
        let mut entries = BTreeMap::new();
        for entry in index.entries() {
            let path = entry.path(&index).to_str_lossy();
            Self::validate(&path)?;
            entries.insert(PathBuf::from(path.as_ref()), Some(entry.id));
        }
        for directory in self.base_directories(repo, root)? {
            Self::validate(&directory)?;
            entries.insert(PathBuf::from(directory), None);
        }
        // Git does not store implicit parent directories as separate entries.
        for path in entries.keys().cloned().collect::<Vec<_>>() {
            for parent in path
                .ancestors()
                .skip(1)
                .filter(|path| !path.as_os_str().is_empty())
            {
                entries.entry(parent.to_path_buf()).or_insert(None);
            }
        }
        Ok(entries)
    }

    fn checkout_matches_commit(
        &self,
        repo: &gix::Repository,
        root: &gix::Tree<'_>,
    ) -> Result<bool, Error> {
        let checkout = self.inner.checkout.root();
        match fs::symlink_metadata(checkout) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(Error::io(checkout, error)),
        }
        let mut expected = self.committed_checkout_entries(repo, root)?;
        let mut directories = vec![checkout.to_path_buf()];
        while let Some(directory) = directories.pop() {
            for entry in fs::read_dir(&directory).map_err(|error| Error::io(&directory, error))? {
                let entry = entry.map_err(|error| Error::io(&directory, error))?;
                let path = entry.path();
                let relative = path
                    .strip_prefix(checkout)
                    .map_err(|_| Error::InvalidPath(path.display().to_string()))?;
                let Some(object) = expected.remove(relative) else {
                    return Ok(false);
                };
                if !Self::checkout_entry_matches(repo, &path, object)? {
                    return Ok(false);
                }
                if object.is_none() {
                    directories.push(path);
                }
            }
        }
        Ok(expected.is_empty())
    }

    fn checkout_entry_matches(
        repo: &gix::Repository,
        path: &Path,
        object: Option<gix::ObjectId>,
    ) -> Result<bool, Error> {
        let metadata = fs::symlink_metadata(path).map_err(|error| Error::io(path, error))?;
        if metadata.file_type().is_symlink() {
            return Ok(false);
        }
        let Some(object) = object else {
            return Ok(metadata.is_dir());
        };
        if !metadata.is_file() {
            return Ok(false);
        }
        let mut file = fs::File::open(path).map_err(|error| Error::io(path, error))?;
        let hash = gix::objs::compute_stream_hash(
            repo.object_hash(),
            gix::objs::Kind::Blob,
            &mut file,
            metadata.len(),
            &mut gix::progress::Discard,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .map_err(|error| Error::git("hash NAND checkout file", error))?;
        Ok(hash == object)
    }

    fn ensure_checkout(&self) -> Result<(), Error> {
        if *self.inner.cache_ready.lock() {
            return Ok(());
        }
        let _guard = self.inner.platform.0.operation.lock();
        let mut ready = self.inner.cache_ready.lock();
        if *ready {
            return Ok(());
        }
        let repo = self.inner.platform.repository().to_thread_local();
        let tip = Self::tip_in(&repo, &self.ref_name())?;
        // Existing NAND checkouts are inspected by native installation checks and repaired only
        // through recover(). Opening one must never erase an interrupted write.
        if self.inner.kind == StorageKind::Nand && tip.is_some() {
            *ready = true;
            return Ok(());
        }
        let root = Self::root_tree(&repo, tip)?;
        let target = self.target_index(&repo, &root)?;
        let current = gix::index::File::at(
            &self.inner.index_path,
            repo.object_hash(),
            false,
            gix::index::decode::Options::default(),
        );
        let reusable = current.as_ref().is_ok_and(|current| {
            self.volume_index_matches(current, &target) && self.checkout_is_complete(&target)
        });
        if !reusable {
            self.checkout_target(&repo, &root, &target)?;
            self.write_index(target)?;
        }
        *ready = true;
        Ok(())
    }

    fn tip_in(repo: &gix::Repository, reference: &str) -> Result<Option<gix::ObjectId>, Error> {
        repo.try_find_reference(reference)
            .map_err(|error| Error::git("find volume reference", error))
            .map(|reference| {
                reference.and_then(|reference| reference.try_id().map(|id| id.detach()))
            })
    }

    fn root_tree(
        repo: &gix::Repository,
        tip: Option<gix::ObjectId>,
    ) -> Result<gix::Tree<'_>, Error> {
        match tip {
            Some(tip) => repo
                .find_commit(tip)
                .map_err(|error| Error::git("find volume commit", error))?
                .tree()
                .map_err(|error| Error::git("read volume tree", error)),
            None => Ok(repo.empty_tree()),
        }
    }

    fn base_directories(
        &self,
        repo: &gix::Repository,
        root: &gix::Tree<'_>,
    ) -> Result<BTreeSet<String>, Error> {
        let Some(entry) = root
            .lookup_entry_by_path("meta/fs/directories")
            .map_err(|error| Error::git("lookup directory metadata", error))?
        else {
            return Ok(BTreeSet::new());
        };
        if entry.mode().kind() != gix::objs::tree::EntryKind::Tree {
            return Err(Error::HistoryConflict(
                "directory metadata is not a tree".to_owned(),
            ));
        }
        let tree = repo
            .find_tree(entry.oid())
            .map_err(|error| Error::git("read directory metadata", error))?;
        let mut directories = BTreeSet::new();
        for entry in tree.iter() {
            let entry = entry.map_err(|error| Error::git("decode directory metadata", error))?;
            if entry.kind() != gix::objs::tree::EntryKind::Blob {
                continue;
            }
            let blob = repo
                .find_blob(entry.oid())
                .map_err(|error| Error::git("read directory path", error))?;
            directories.insert(
                String::from_utf8(blob.data.clone())
                    .map_err(|error| Error::git("decode directory path", error))?,
            );
        }
        Ok(directories)
    }

    pub(crate) fn commit_pending(
        &self,
        message: &str,
        extra_parent: Option<gix::ObjectId>,
    ) -> Result<Option<gix::ObjectId>, Error> {
        let _guard = self.inner.platform.0.operation.lock();
        let reference = self.ref_name();
        let repo = self.inner.platform.repository().to_thread_local();
        let parent = Self::tip_in(&repo, &reference)?;
        let mut state = self.inner.working.write();
        if extra_parent.is_none() && state.is_empty() {
            return Ok(None);
        }
        let root = Self::root_tree(&repo, parent)?;
        let root_id = root.id;
        let mut editor = root
            .edit()
            .map_err(|error| Error::git("edit volume tree", error))?;

        for (path, entry) in &state.entries {
            let git_path = format!("files/{path}");
            match entry {
                OverlayEntry::File => {
                    let source = self
                        .inner
                        .checkout
                        .root()
                        .join(path.replace('/', std::path::MAIN_SEPARATOR_STR));
                    let mut file =
                        fs::File::open(&source).map_err(|error| Error::io(&source, error))?;
                    let length = file
                        .metadata()
                        .map_err(|error| Error::io(&source, error))?
                        .len();
                    let id = repo
                        .objects
                        .write_stream(gix::objs::Kind::Blob, length, &mut file)
                        .map_err(|error| Error::git("write volume blob", error))?;
                    editor
                        .upsert(git_path, gix::objs::tree::EntryKind::Blob, id)
                        .map_err(|error| Error::git("update volume file", error))?;
                }
                OverlayEntry::Directory => {
                    let key = directory_key(path);
                    let id = repo
                        .write_blob(path.as_bytes())
                        .map_err(|error| Error::git("write directory metadata", error))?
                        .detach();
                    editor
                        .upsert(
                            format!("meta/fs/directories/{key}"),
                            gix::objs::tree::EntryKind::Blob,
                            id,
                        )
                        .map_err(|error| Error::git("update directory metadata", error))?;
                }
                OverlayEntry::Deleted => {
                    editor
                        .remove(&git_path)
                        .map_err(|error| Error::git("remove volume path", error))?;
                    let directories = self.base_directories(&repo, &root)?;
                    for directory in directories.iter().filter(|directory| {
                        *directory == path || directory.starts_with(&format!("{path}/"))
                    }) {
                        editor
                            .remove_leaf(format!(
                                "meta/fs/directories/{}",
                                directory_key(directory)
                            ))
                            .map_err(|error| Error::git("remove directory metadata", error))?;
                    }
                }
            }
        }

        if let Some(info) = &state.metadata.volume {
            write_text(&repo, &mut editor, "meta/volume/id", &info.id.to_string())?;
            write_text(
                &repo,
                &mut editor,
                "meta/volume/kind",
                self.inner.kind.metadata_name(),
            )?;
            write_text(&repo, &mut editor, "meta/volume/platform", &info.platform)?;
            write_text(
                &repo,
                &mut editor,
                "meta/volume/created",
                &info.created.as_second().to_string(),
            )?;
            match &info.label {
                Some(label) => write_text(&repo, &mut editor, "meta/volume/label", label)?,
                None => {
                    editor
                        .remove_leaf("meta/volume/label")
                        .map_err(|error| Error::git("remove volume label", error))?;
                }
            }
        }

        for (artifact_id, artifact) in &state.metadata.artifacts {
            let root = format!("meta/artifacts/{}", artifact_key(artifact_id));
            editor
                .remove(&root)
                .map_err(|error| Error::git("replace artifact metadata", error))?;
            if let Some(artifact) = artifact {
                artifact.write(&repo, &mut editor, &root)?;
            }
        }

        for (depot, tip) in &state.metadata.save_tips {
            let path = format!("meta/resources/saves/{depot:016x}/head");
            match tip {
                Some(tip) => write_text(&repo, &mut editor, &path, &tip.to_string())?,
                None => {
                    editor
                        .remove(&format!("meta/resources/saves/{depot:016x}"))
                        .map_err(|error| Error::git("remove profile save resource", error))?;
                }
            }
        }

        let tree = editor
            .write()
            .map_err(|error| Error::git("write volume tree", error))?
            .detach();
        if extra_parent.is_none() && tree == root_id {
            self.write_index(self.target_index(&repo, &root)?)?;
            *self.inner.cache_ready.lock() = true;
            *state = WorkingState::default();
            return Ok(None);
        }
        let signature = signature();
        let mut time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut time);
        let mut parents = parent.into_iter().collect::<Vec<_>>();
        if let Some(extra_parent) = extra_parent {
            parents.push(extra_parent);
        }
        let commit = repo
            .new_commit_as(signature, signature, message, tree, parents)
            .map_err(|error| Error::git("write volume commit", error))?
            .id;
        let expected = parent.map_or(PreviousValue::MustNotExist, |id| {
            PreviousValue::ExistingMustMatch(Target::Object(id))
        });
        let edit = RefEdit {
            change: Change::Update {
                log: LogChange {
                    mode: RefLog::AndReference,
                    force_create_reflog: true,
                    message: message.into(),
                },
                expected,
                new: Target::Object(commit),
            },
            name: gix::refs::FullName::try_from(reference.as_str())
                .map_err(|error| Error::git("validate volume reference", error))?,
            deref: false,
        };
        repo.edit_references_as([edit], Some(signature))
            .map_err(|error| Error::git("publish volume commit", error))?;
        let committed_root = repo
            .find_tree(tree)
            .map_err(|error| Error::git("read committed volume tree", error))?;
        fs::create_dir_all(self.inner.checkout.root())
            .map_err(|error| Error::io(self.inner.checkout.root(), error))?;
        self.write_index(self.target_index(&repo, &committed_root)?)?;
        *self.inner.cache_ready.lock() = true;
        *state = WorkingState::default();
        Ok(Some(commit))
    }

    fn read_value(&self, path: &str) -> Result<Option<Vec<u8>>, Error> {
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

    pub(crate) fn set_save_tip(&self, depot: u64, tip: Option<gix::ObjectId>) {
        self.inner
            .working
            .write()
            .metadata
            .save_tips
            .insert(depot, tip);
    }

    fn volume_info_sync(&self) -> Result<Option<VolumeInfo>, Error> {
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

impl Storage for GitVolumeStorage {
    fn exists(&self, path: &str) -> bool {
        self.ensure_checkout().is_ok() && self.inner.checkout.exists(path)
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        Self::validate(path)?;
        self.ensure_checkout()?;
        self.inner.checkout.metadata(path)
    }

    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        Self::validate(path)?;
        self.ensure_checkout()?;
        self.inner.checkout.read_dir(path, recursive)
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        Self::validate(path)?;
        self.ensure_checkout()?;
        self.inner.checkout.create_dir_all(path)?;
        if path.is_empty() {
            return Ok(());
        }
        let mut current = String::new();
        let mut working = self.inner.working.write();
        for component in path.split('/') {
            current = join(&current, component);
            working
                .entries
                .insert(current.clone(), OverlayEntry::Directory);
        }
        drop(working);
        Ok(())
    }

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        Self::validate(path)?;
        if path.is_empty() {
            return Err(Error::InvalidPath(path.to_owned()));
        }
        self.ensure_checkout()?;
        let file = self.inner.checkout.create(path)?;
        self.inner
            .working
            .write()
            .entries
            .insert(path.to_owned(), OverlayEntry::File);
        self.inner.changes.notify_one();
        Ok(Arc::new(GitVolumeFile::new(
            file,
            self.inner.changes.clone(),
        )))
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        Self::validate(path)?;
        self.ensure_checkout()?;
        let file = self.inner.checkout.open(path, write)?;
        if write {
            self.inner
                .working
                .write()
                .entries
                .insert(path.to_owned(), OverlayEntry::File);
        }
        if write {
            Ok(Arc::new(GitVolumeFile::new(
                file,
                self.inner.changes.clone(),
            )))
        } else {
            Ok(file)
        }
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        Self::validate(path)?;
        if path.is_empty() {
            return Err(Error::InvalidPath(path.to_owned()));
        }
        self.ensure_checkout()?;
        self.inner.checkout.remove(path)?;
        self.inner
            .working
            .write()
            .entries
            .insert(path.to_owned(), OverlayEntry::Deleted);
        self.inner.changes.notify_one();
        Ok(())
    }

    fn watch(&self) -> Result<Option<Box<dyn ChangeSignal>>, Error> {
        self.ensure_checkout()?;
        Ok(Some(Box::new(self.inner.changes.clone())))
    }

    fn install_file<'a>(
        &'a self,
        source: Box<dyn AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        let dest = dest.to_owned();
        Box::pin(async move {
            Self::validate(&dest)?;
            self.ensure_checkout()?;
            let result = self.inner.checkout.install_file(source, &dest).await?;
            self.inner
                .working
                .write()
                .entries
                .insert(dest, OverlayEntry::File);
            self.inner.changes.notify_one();
            Ok(result)
        })
    }
}

impl MountableStorage for GitVolumeStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        Self::validate(from)?;
        Self::validate(to)?;
        self.ensure_checkout()?;
        let metadata = self.metadata(from)?;
        let descendants = if metadata.kind == EntryKind::Directory {
            self.inner
                .checkout
                .read_dir(from, true)?
                .map(|entry| (entry.path, entry.metadata.kind))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        self.inner.checkout.rename(from, to)?;
        let mut working = self.inner.working.write();
        working
            .entries
            .insert(from.to_owned(), OverlayEntry::Deleted);
        if metadata.kind == EntryKind::File {
            working.entries.insert(to.to_owned(), OverlayEntry::File);
        } else {
            working
                .entries
                .insert(to.to_owned(), OverlayEntry::Directory);
            for (path, kind) in descendants {
                let relative = path
                    .strip_prefix(from)
                    .expect("descendant begins with its requested root")
                    .trim_start_matches('/');
                let target = join(to, relative);
                working.entries.insert(path, OverlayEntry::Deleted);
                working.entries.insert(
                    target,
                    match kind {
                        EntryKind::Directory => OverlayEntry::Directory,
                        EntryKind::File => OverlayEntry::File,
                    },
                );
            }
        }
        drop(working);
        self.inner.changes.notify_one();
        Ok(())
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        self.open(path, true)?.set_len(len)
    }

    fn sync(&self, _path: &str) -> Result<(), Error> {
        self.ensure_checkout()?;
        self.inner.checkout.sync(_path)
    }

    fn finish(&self, _path: &str) -> Result<(), Error> {
        // Commits and explicit recovery define the persistent state. Unmounting
        // must leave uncommitted NAND changes available for inspection.
        Ok(())
    }
}

impl ChangeSignal for Arc<Notify> {
    fn changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.notified())
    }
}

struct GitVolumeFile {
    file: Arc<dyn StorageFile>,
    changes: Arc<Notify>,
}

impl GitVolumeFile {
    fn new(file: Arc<dyn StorageFile>, changes: Arc<Notify>) -> Self {
        Self { file, changes }
    }
}

impl StorageFile for GitVolumeFile {
    fn metadata(&self) -> Result<Metadata, Error> {
        self.file.metadata()
    }

    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
        self.file.read_at(offset, buffer)
    }

    fn write_at(&self, offset: u64, buffer: &[u8]) -> Result<usize, Error> {
        let written = self.file.write_at(offset, buffer)?;
        if written != 0 {
            self.changes.notify_one();
        }
        Ok(written)
    }

    fn set_len(&self, len: u64) -> Result<(), Error> {
        self.file.set_len(len)?;
        self.changes.notify_one();
        Ok(())
    }

    fn flush(&self) -> Result<(), Error> {
        self.file.flush()?;
        self.changes.notify_one();
        Ok(())
    }

    fn sync(&self) -> Result<(), Error> {
        self.file.sync()?;
        self.changes.notify_one();
        Ok(())
    }
}

impl VolumeStorage for GitVolumeStorage {
    fn is_dirty(&self) -> Result<bool, Error> {
        let _guard = self.inner.platform.0.operation.lock();
        let repo = self.inner.platform.repository().to_thread_local();
        let tip = Self::tip_in(&repo, &self.ref_name())?;
        let root = Self::root_tree(&repo, tip)?;
        if !self.inner.working.read().is_empty() {
            return Ok(true);
        }
        Ok(!self.checkout_matches_commit(&repo, &root)?)
    }

    fn recover(&self) -> Result<(), Error> {
        if self.inner.kind != StorageKind::Nand {
            return Err(Error::Unsupported("Only NANDs support recovery".into()));
        }
        let _guard = self.inner.platform.0.operation.lock();
        let mut state = self.inner.working.write();
        let repo = self.inner.platform.repository().to_thread_local();
        let tip = Self::tip_in(&repo, &self.ref_name())?.ok_or(Error::UnclaimedNand)?;
        self.refresh_checkout_locked(&repo, Some(tip))?;
        *state = WorkingState::default();
        Ok(())
    }

    fn label(&self) -> BoxFuture<'_, Result<Option<String>, Error>> {
        Box::pin(async move { Ok(self.volume_info_sync()?.and_then(|info| info.label)) })
    }

    fn set_label<'a>(&'a self, label: Option<&'a str>) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            let mut info = self.volume_info_sync()?.ok_or(Error::UnclaimedNand)?;
            if info.label.as_deref() == label {
                return Ok(());
            }
            info.label = label.map(str::to_owned);
            self.inner.working.write().metadata.volume = Some(info);
            self.commit_pending("Polymer volume label changed", None)?;
            Ok(())
        })
    }

    fn artifacts(&self) -> &dyn ArtifactRegistry {
        &self.artifacts
    }

    fn volumeinfo(&self) -> BoxFuture<'_, Result<Option<VolumeInfo>, Error>> {
        Box::pin(async move { self.volume_info_sync() })
    }

    fn brand<'a>(
        &'a self,
        id: Uuid,
        platform: &'a str,
        label: Option<&'a str>,
    ) -> BoxFuture<'a, Result<VolumeInfo, Error>> {
        Box::pin(async move {
            if let Some(info) = self.volume_info_sync()? {
                return Ok(info);
            }
            if id != self.inner.id || platform != self.inner.platform.platform() {
                return Err(Error::HistoryConflict(
                    "volume identity does not match its ref".to_owned(),
                ));
            }
            let info = VolumeInfo {
                id,
                platform: platform.to_owned(),
                label: label.map(str::to_owned),
                created: Timestamp::from_second(Timestamp::now().as_second())?,
            };
            self.inner.working.write().metadata.volume = Some(info.clone());
            self.commit_pending("Polymer volume created", None)?;
            Ok(info)
        })
    }
}

#[derive(Clone)]
struct ArtifactRecord {
    id: String,
    depot_type: DepotType,
    manifest: Vec<ArtifactFileRecord>,
    label: String,
    version: Option<String>,
    icon_path: Option<PathBuf>,
    icon_png: Option<Arc<[u8]>>,
    byte_length: u64,
}

#[derive(Clone)]
struct ArtifactFileRecord {
    path: PathBuf,
    blake3: [u8; 32],
    byte_length: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TreeLeaf {
    kind: gix::objs::tree::EntryKind,
    id: gix::ObjectId,
}

#[derive(Clone)]
struct NandCommit {
    id: gix::ObjectId,
    commit: gix::objs::Commit,
    artifact: Option<String>,
}

impl From<&Artifact> for ArtifactRecord {
    fn from(artifact: &Artifact) -> Self {
        Self {
            id: artifact.id().to_owned(),
            depot_type: artifact.depot_type(),
            manifest: artifact
                .manifest()
                .iter()
                .map(|file| ArtifactFileRecord {
                    path: file.path().to_owned(),
                    blake3: *file.blake3(),
                    byte_length: file.byte_length(),
                })
                .collect(),
            label: artifact.label().to_owned(),
            version: artifact.version().map(str::to_owned),
            icon_path: match artifact.icon() {
                IconHint::File(path) => Some(path.clone()),
                IconHint::Builtin(_) | IconHint::EmbeddedPng(_) => None,
            },
            icon_png: match artifact.icon() {
                IconHint::EmbeddedPng(bytes) => Some(bytes.clone()),
                _ => None,
            },
            byte_length: artifact.byte_length(),
        }
    }
}

impl ArtifactRecord {
    fn artifact(&self) -> Artifact {
        Artifact::new(
            self.id.clone(),
            self.depot_type,
            self.manifest
                .iter()
                .map(|file| DepotFile::new(file.path.clone(), file.blake3, file.byte_length))
                .collect(),
            self.label.clone(),
            self.version.clone(),
            self.icon_png
                .clone()
                .map(IconHint::EmbeddedPng)
                .or_else(|| self.icon_path.clone().map(IconHint::File))
                .unwrap_or(IconHint::Builtin(self.depot_type)),
        )
    }

    fn write(
        &self,
        repo: &gix::Repository,
        editor: &mut gix::object::tree::Editor<'_>,
        root: &str,
    ) -> Result<(), Error> {
        write_text(repo, editor, &format!("{root}/id"), &self.id)?;
        write_text(
            repo,
            editor,
            &format!("{root}/depot-type"),
            &u8::from(self.depot_type).to_string(),
        )?;
        write_text(repo, editor, &format!("{root}/label"), &self.label)?;
        write_text(
            repo,
            editor,
            &format!("{root}/byte-length"),
            &self.byte_length.to_string(),
        )?;
        if let Some(version) = &self.version {
            write_text(repo, editor, &format!("{root}/version"), version)?;
        }
        if let Some(path) = &self.icon_path {
            write_text(
                repo,
                editor,
                &format!("{root}/icon-path"),
                &path.to_string_lossy(),
            )?;
        }
        if let Some(png) = &self.icon_png {
            let blob = repo
                .write_blob(png.as_ref())
                .map_err(|error| Error::git("write artifact icon", error))?
                .detach();
            editor
                .upsert(
                    format!("{root}/icon.png"),
                    gix::objs::tree::EntryKind::Blob,
                    blob,
                )
                .map_err(|error| Error::git("update artifact icon", error))?;
        }
        for file in &self.manifest {
            let path = file.path.to_string_lossy().replace('\\', "/");
            let file_root = format!("{root}/files/{}", directory_key(&path));
            write_text(repo, editor, &format!("{file_root}/path"), &path)?;
            write_text(
                repo,
                editor,
                &format!("{file_root}/byte-length"),
                &file.byte_length.to_string(),
            )?;
            let digest = repo
                .write_blob(file.blake3)
                .map_err(|error| Error::git("write artifact digest", error))?
                .detach();
            editor
                .upsert(
                    format!("{file_root}/blake3"),
                    gix::objs::tree::EntryKind::Blob,
                    digest,
                )
                .map_err(|error| Error::git("update artifact digest", error))?;
        }
        Ok(())
    }
}

#[derive(Clone)]
struct GitArtifactRegistry(Arc<GitVolumeInner>);

impl GitArtifactRegistry {
    fn storage(&self) -> GitVolumeStorage {
        GitVolumeStorage {
            inner: self.0.clone(),
            artifacts: self.clone(),
        }
    }

    fn records(&self) -> Result<Vec<ArtifactRecord>, Error> {
        let storage = self.storage();
        let repo = self.0.platform.repository().to_thread_local();
        let tip = GitVolumeStorage::tip_in(&repo, &storage.ref_name())?;
        let tree = GitVolumeStorage::root_tree(&repo, tip)?;
        let Some(artifacts) = tree
            .lookup_entry_by_path("meta/artifacts")
            .map_err(|error| Error::git("lookup artifacts", error))?
        else {
            return Ok(Vec::new());
        };
        let artifacts = repo
            .find_tree(artifacts.oid())
            .map_err(|error| Error::git("read artifacts", error))?;
        let mut records = Vec::new();
        for entry in artifacts.iter() {
            let entry = entry.map_err(|error| Error::git("decode artifact entry", error))?;
            if entry.kind() != gix::objs::tree::EntryKind::Tree {
                continue;
            }
            records.push(read_artifact(&repo, entry.oid().into())?);
        }
        Ok(records)
    }

    fn artifact_from_message(message: &[u8]) -> Option<String> {
        message
            .as_bstr()
            .to_str_lossy()
            .lines()
            .next()?
            .strip_prefix("Polymer install ")
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
    }

    fn linear_history(
        repo: &gix::Repository,
        tip: gix::ObjectId,
    ) -> Result<Vec<NandCommit>, Error> {
        let mut current = Some(tip);
        let mut history = Vec::new();
        while let Some(id) = current {
            let object = repo
                .find_commit(id)
                .map_err(|error| Error::git("read NAND commit", error))?;
            let decoded = object
                .decode()
                .map_err(|error| Error::git("decode NAND commit", error))?;
            let commit = gix::objs::Commit::try_from(decoded)
                .map_err(|error| Error::git("own NAND commit", error))?;
            if commit.parents.len() > 1 {
                return Err(Error::HistoryConflict(
                    "NAND history must be linear to uninstall depots".to_owned(),
                ));
            }
            current = commit.parents.first().copied();
            history.push(NandCommit {
                id,
                artifact: Self::artifact_from_message(&commit.message),
                commit,
            });
        }
        history.reverse();
        Ok(history)
    }

    fn tree_snapshot(
        repo: &gix::Repository,
        tree: gix::ObjectId,
    ) -> Result<BTreeMap<String, TreeLeaf>, Error> {
        let tree = repo
            .find_tree(tree)
            .map_err(|error| Error::git("read NAND tree", error))?;
        let entries = tree
            .traverse()
            .breadthfirst
            .files()
            .map_err(|error| Error::git("traverse NAND tree", error))?;
        let mut snapshot = BTreeMap::new();
        for entry in entries {
            let kind = entry.mode.kind();
            if kind == gix::objs::tree::EntryKind::Tree {
                continue;
            }
            if !matches!(
                kind,
                gix::objs::tree::EntryKind::Blob | gix::objs::tree::EntryKind::BlobExecutable
            ) {
                return Err(Error::HistoryConflict(format!(
                    "unsupported entry in NAND tree: {}",
                    entry.filepath.to_str_lossy()
                )));
            }
            snapshot.insert(
                entry.filepath.to_str_lossy().into_owned(),
                TreeLeaf {
                    kind,
                    id: entry.oid,
                },
            );
        }
        Ok(snapshot)
    }

    fn empty_snapshot() -> BTreeMap<String, TreeLeaf> {
        BTreeMap::new()
    }

    fn parent_snapshot(
        repo: &gix::Repository,
        commit: &NandCommit,
    ) -> Result<BTreeMap<String, TreeLeaf>, Error> {
        match commit.commit.parents.first() {
            Some(parent) => repo
                .find_commit(*parent)
                .map_err(|error| Error::git("read NAND parent", error))?
                .tree_id()
                .map_err(|error| Error::git("read NAND parent tree", error))
                .and_then(|tree| Self::tree_snapshot(repo, tree.detach())),
            None => Ok(Self::empty_snapshot()),
        }
    }

    fn apply_delta(
        repo: &gix::Repository,
        commit: &NandCommit,
        current: &mut BTreeMap<String, TreeLeaf>,
    ) -> Result<Vec<String>, Error> {
        let before = Self::parent_snapshot(repo, commit)?;
        let after = Self::tree_snapshot(repo, commit.commit.tree)?;
        let paths = before
            .keys()
            .chain(after.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut updated = current.clone();
        let mut conflicts = Vec::new();
        for path in paths {
            let before = before.get(&path).copied();
            let after = after.get(&path).copied();
            if before == after {
                continue;
            }
            let found = current.get(&path).copied();
            if found != before && found != after {
                conflicts.push(path);
                continue;
            }
            match after {
                Some(entry) => {
                    updated.insert(path, entry);
                }
                None => {
                    updated.remove(&path);
                }
            }
        }
        if conflicts.is_empty() {
            *current = updated;
        }
        Ok(conflicts)
    }

    fn write_snapshot(
        repo: &gix::Repository,
        snapshot: &BTreeMap<String, TreeLeaf>,
    ) -> Result<gix::ObjectId, Error> {
        let mut editor = repo
            .empty_tree()
            .edit()
            .map_err(|error| Error::git("edit rewritten NAND tree", error))?;
        for (path, entry) in snapshot {
            editor
                .upsert(path, entry.kind, entry.id)
                .map_err(|error| Error::git("update rewritten NAND tree", error))?;
        }
        editor
            .write()
            .map(|id| id.detach())
            .map_err(|error| Error::git("write rewritten NAND tree", error))
    }

    fn manifest_dependencies(
        records: &[ArtifactRecord],
        selected: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        let selected_paths = records
            .iter()
            .filter(|record| selected.contains(&record.id))
            .flat_map(|record| record.manifest.iter())
            .map(|file| file.path.to_string_lossy().replace('\\', "/"))
            .collect::<BTreeSet<_>>();
        records
            .iter()
            .filter(|record| !selected.contains(&record.id))
            .filter(|record| {
                record.manifest.iter().any(|file| {
                    selected_paths.contains(&file.path.to_string_lossy().replace('\\', "/"))
                })
            })
            .map(|record| record.id.clone())
            .collect()
    }

    fn affected_depots(
        repo: &gix::Repository,
        history: &[NandCommit],
        records: &[ArtifactRecord],
        requested: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, Error> {
        let installed = records
            .iter()
            .map(|record| record.id.clone())
            .collect::<BTreeSet<_>>();
        let mut selected = requested
            .intersection(&installed)
            .cloned()
            .collect::<BTreeSet<_>>();
        if selected.is_empty() {
            return Ok(BTreeSet::new());
        }
        loop {
            let mut added = Self::manifest_dependencies(records, &selected);
            let first = history
                .iter()
                .position(|commit| {
                    commit
                        .artifact
                        .as_ref()
                        .is_some_and(|id| selected.contains(id))
                })
                .ok_or_else(|| {
                    Error::HistoryConflict(format!(
                        "no installation commit was found for {}",
                        selected.iter().cloned().collect::<Vec<_>>().join(", ")
                    ))
                })?;
            for id in &selected {
                if !history
                    .iter()
                    .any(|commit| commit.artifact.as_ref() == Some(id))
                {
                    return Err(Error::HistoryConflict(format!(
                        "no installation commit was found for {id}"
                    )));
                }
            }
            let mut current = Self::parent_snapshot(repo, &history[first])?;
            for commit in &history[first..] {
                if commit
                    .artifact
                    .as_ref()
                    .is_some_and(|id| selected.contains(id))
                {
                    continue;
                }
                let conflicts = Self::apply_delta(repo, commit, &mut current)?;
                if conflicts.is_empty() {
                    continue;
                }
                if let Some(id) = &commit.artifact {
                    added.insert(id.clone());
                    break;
                } else {
                    return Err(Error::HistoryConflict(format!(
                        "dropping depot commits conflicts with commit {} at {}",
                        commit.id,
                        conflicts.join(", ")
                    )));
                }
            }
            added.retain(|id| !selected.contains(id));
            if added.is_empty() {
                return Ok(selected.difference(requested).cloned().collect());
            }
            selected.extend(added);
        }
    }

    fn rewrite_without(
        &self,
        repo: &gix::Repository,
        history: &[NandCommit],
        removed: &BTreeSet<String>,
        old_tip: gix::ObjectId,
    ) -> Result<(), Error> {
        let first = history
            .iter()
            .position(|commit| {
                commit
                    .artifact
                    .as_ref()
                    .is_some_and(|id| removed.contains(id))
            })
            .ok_or_else(|| {
                Error::HistoryConflict("no installation commits were selected".to_owned())
            })?;
        let mut current = Self::parent_snapshot(repo, &history[first])?;
        let mut parent = history[first].commit.parents.first().copied();
        for original in &history[first..] {
            if original
                .artifact
                .as_ref()
                .is_some_and(|id| removed.contains(id))
            {
                continue;
            }
            let conflicts = Self::apply_delta(repo, original, &mut current)?;
            if !conflicts.is_empty() {
                return Err(Error::HistoryConflict(format!(
                    "rewriting commit {} conflicts at {}",
                    original.id,
                    conflicts.join(", ")
                )));
            }
            let mut rewritten = original.commit.clone();
            rewritten.tree = Self::write_snapshot(repo, &current)?;
            rewritten.parents.clear();
            if let Some(parent) = parent {
                rewritten.parents.push(parent);
            }
            parent = Some(
                repo.write_object(rewritten)
                    .map_err(|error| Error::git("write rebased NAND commit", error))?
                    .detach(),
            );
        }
        let new_tip = parent.ok_or_else(|| {
            Error::HistoryConflict("uninstall cannot remove the entire NAND history".to_owned())
        })?;
        let reference = self.storage().ref_name();
        let name = gix::refs::FullName::try_from(reference.as_str())
            .map_err(|error| Error::git("validate NAND reference", error))?;
        let signature = signature();
        let mut time = gix::date::parse::TimeBuf::default();
        let signature = signature.to_ref(&mut time);
        repo.edit_references_as(
            [RefEdit {
                change: Change::Update {
                    log: LogChange {
                        mode: RefLog::AndReference,
                        force_create_reflog: true,
                        message: format!(
                            "Polymer uninstall {}",
                            removed.iter().cloned().collect::<Vec<_>>().join(" ")
                        )
                        .into(),
                    },
                    expected: PreviousValue::MustExistAndMatch(Target::Object(old_tip)),
                    new: Target::Object(new_tip),
                },
                name: name.clone(),
                deref: false,
            }],
            Some(signature),
        )
        .map_err(|error| Error::git("publish rewritten NAND history", error))?;
        repo.edit_references_as(
            [RefEdit {
                change: Change::Delete {
                    expected: PreviousValue::MustExistAndMatch(Target::Object(new_tip)),
                    log: RefLog::Only,
                },
                name,
                deref: false,
            }],
            Some(signature),
        )
        .map_err(|error| Error::git("clear rewritten NAND reflog", error))?;
        self.storage()
            .refresh_checkout_locked(repo, Some(new_tip))?;
        Ok(())
    }
}

impl ArtifactRegistry for GitArtifactRegistry {
    fn revision(&self) -> Result<String, Error> {
        let repo = self.0.platform.repository().to_thread_local();
        Ok(GitVolumeStorage::tip_in(&repo, &self.storage().ref_name())?
            .map(|id| id.to_string())
            .unwrap_or_default())
    }
    fn affected<'a>(&'a self, ids: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<String>, Error>> {
        Box::pin(async move {
            let _guard = self.0.platform.0.operation.lock();
            if !self.0.working.read().is_empty() {
                return Err(Error::HistoryConflict(
                    "NAND has uncommitted changes".into(),
                ));
            }
            let repo = self.0.platform.repository().to_thread_local();
            let Some(tip) = GitVolumeStorage::tip_in(&repo, &self.storage().ref_name())? else {
                return Ok(Vec::new());
            };
            let history = Self::linear_history(&repo, tip)?;
            let records = self.records()?;
            let installed = records
                .iter()
                .map(|record| record.id.clone())
                .collect::<BTreeSet<_>>();
            let removed = ids
                .iter()
                .filter(|id| installed.contains(**id))
                .map(|id| (*id).to_owned())
                .collect::<BTreeSet<_>>();
            Ok(Self::affected_depots(&repo, &history, &records, &removed)?
                .into_iter()
                .collect())
        })
    }

    fn record<'a>(&'a self, artifact: &'a Artifact) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            self.0.working.write().metadata.artifacts.insert(
                artifact.id().to_owned(),
                Some(ArtifactRecord::from(artifact)),
            );
            self.storage()
                .commit_pending(&format!("Polymer install {}", artifact.id()), None)?;
            Ok(())
        })
    }

    fn summaries(&self) -> BoxFuture<'_, Result<Vec<ArtifactSummary>, Error>> {
        Box::pin(async move {
            let mut summaries = self
                .records()?
                .into_iter()
                .map(|record| ArtifactSummary {
                    id: record.id,
                    depot_type: record.depot_type,
                    label: record.label,
                    version: record.version,
                    byte_length: record.byte_length,
                })
                .collect::<Vec<_>>();
            summaries.sort_by(|left, right| left.id.cmp(&right.id));
            Ok(summaries)
        })
    }

    fn artifact<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Artifact>, Error>> {
        Box::pin(async move {
            Ok(self
                .records()?
                .into_iter()
                .find(|record| record.id == id)
                .map(|record| record.artifact()))
        })
    }

    fn contains<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, Error>> {
        Box::pin(async move { Ok(self.records()?.iter().any(|record| record.id == id)) })
    }

    fn uninstall<'a>(&'a self, ids: &'a [&'a str]) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            let requested = ids
                .iter()
                .map(|id| (*id).to_owned())
                .collect::<BTreeSet<_>>();
            if requested.is_empty() {
                return Ok(());
            }
            let _guard = self.0.platform.0.operation.lock();
            if !self.0.working.read().is_empty() {
                return Err(Error::HistoryConflict(
                    "cannot uninstall depots while the NAND has uncommitted changes".to_owned(),
                ));
            }
            let repo = self.0.platform.repository().to_thread_local();
            let storage = self.storage();
            let Some(tip) = GitVolumeStorage::tip_in(&repo, &storage.ref_name())? else {
                return Ok(());
            };
            let records = self.records()?;
            let installed = records
                .iter()
                .map(|record| record.id.clone())
                .collect::<BTreeSet<_>>();
            let removed = requested
                .intersection(&installed)
                .cloned()
                .collect::<BTreeSet<_>>();
            if removed.is_empty() {
                return Ok(());
            }
            let history = Self::linear_history(&repo, tip)?;
            let affected = Self::affected_depots(&repo, &history, &records, &removed)?;
            if !affected.is_empty() {
                return Err(Error::AffectedDepots {
                    affected: affected.into_iter().collect(),
                });
            }
            self.rewrite_without(&repo, &history, &removed, tip)
        })
    }
}

fn read_artifact(repo: &gix::Repository, tree: gix::ObjectId) -> Result<ArtifactRecord, Error> {
    let tree = repo
        .find_tree(tree)
        .map_err(|error| Error::git("read artifact tree", error))?;
    let value = |path: &str| -> Result<Option<Vec<u8>>, Error> {
        let Some(entry) = tree
            .lookup_entry_by_path(path)
            .map_err(|error| Error::git("lookup artifact value", error))?
        else {
            return Ok(None);
        };
        repo.find_blob(entry.oid())
            .map(|blob| Some(blob.data.clone()))
            .map_err(|error| Error::git("read artifact value", error))
    };
    let text = |path: &str| -> Result<String, Error> {
        String::from_utf8(
            value(path)?
                .ok_or_else(|| Error::HistoryConflict(format!("artifact has no `{path}`")))?,
        )
        .map_err(|error| Error::git("decode artifact value", error))
    };
    let id = text("id")?;
    let depot_type = text("depot-type")?
        .parse::<u8>()
        .map_err(|error| Error::git("parse artifact type", error))?;
    let depot_type = DepotType::try_from(depot_type)
        .map_err(|value| Error::HistoryConflict(format!("invalid artifact type {value}")))?;
    let label = text("label")?;
    let byte_length = text("byte-length")?
        .parse::<u64>()
        .map_err(|error| Error::git("parse artifact size", error))?;
    let version = value("version")?
        .map(String::from_utf8)
        .transpose()
        .map_err(|error| Error::git("decode artifact version", error))?;
    let icon_path = value("icon-path")?
        .map(String::from_utf8)
        .transpose()
        .map_err(|error| Error::git("decode artifact icon path", error))?
        .map(PathBuf::from);
    let mut manifest = Vec::new();
    if let Some(files) = tree
        .lookup_entry_by_path("files")
        .map_err(|error| Error::git("lookup artifact files", error))?
    {
        let files = repo
            .find_tree(files.oid())
            .map_err(|error| Error::git("read artifact files", error))?;
        for file in files.iter() {
            let file = file.map_err(|error| Error::git("decode artifact file", error))?;
            let file = repo
                .find_tree(file.oid())
                .map_err(|error| Error::git("read artifact file", error))?;
            let file_value = |name: &str| -> Result<Vec<u8>, Error> {
                let entry = file
                    .lookup_entry_by_path(name)
                    .map_err(|error| Error::git("lookup artifact file value", error))?
                    .ok_or_else(|| {
                        Error::HistoryConflict(format!("artifact file has no `{name}`"))
                    })?;
                repo.find_blob(entry.oid())
                    .map(|blob| blob.data.clone())
                    .map_err(|error| Error::git("read artifact file value", error))
            };
            let path = PathBuf::from(
                String::from_utf8(file_value("path")?)
                    .map_err(|error| Error::git("decode artifact file path", error))?,
            );
            let blake3: [u8; 32] = file_value("blake3")?.try_into().map_err(|_| {
                Error::HistoryConflict("artifact digest has the wrong length".to_owned())
            })?;
            let byte_length = String::from_utf8(file_value("byte-length")?)
                .map_err(|error| Error::git("decode artifact file size", error))?
                .parse::<u64>()
                .map_err(|error| Error::git("parse artifact file size", error))?;
            manifest.push(ArtifactFileRecord {
                path,
                blake3,
                byte_length,
            });
        }
    }
    Ok(ArtifactRecord {
        id,
        depot_type,
        manifest,
        label,
        version,
        icon_path,
        icon_png: value("icon.png")?.map(Arc::from),
        byte_length,
    })
}

fn write_text(
    repo: &gix::Repository,
    editor: &mut gix::object::tree::Editor<'_>,
    path: &str,
    value: &str,
) -> Result<(), Error> {
    let blob = repo
        .write_blob(value.as_bytes())
        .map_err(|error| Error::git("write metadata value", error))?
        .detach();
    editor
        .upsert(path, gix::objs::tree::EntryKind::Blob, blob)
        .map(|_| ())
        .map_err(|error| Error::git("update metadata value", error))
}

fn signature() -> gix::actor::Signature {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let time = format!("{seconds} +0000");
    gix::actor::SignatureRef {
        name: BStr::new(b"Polymer"),
        email: BStr::new(b"storage@polymer.invalid"),
        time: &time,
    }
    .to_owned()
    .expect("Polymer generates a valid Git signature")
}

fn join(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_owned()
    } else if child.is_empty() {
        parent.to_owned()
    } else {
        format!("{parent}/{child}")
    }
}

fn directory_key(path: &str) -> String {
    blake3::hash(path.as_bytes()).to_hex().to_string()
}

fn artifact_key(id: &str) -> String {
    blake3::hash(id.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};

    #[tokio::test]
    async fn platform_repo_keeps_multiple_lazy_git_volumes() {
        let root = tempfile::tempdir().unwrap();
        let platform = PlatformRepository::open(root.path().join("TEST.git"), "TEST").unwrap();
        let first = platform.volume(StorageKind::Nand, Uuid::now_v7());
        let second = platform.volume(StorageKind::Nand, Uuid::now_v7());
        first
            .install_file(Box::new(Cursor::new(b"one")), "deep/game/content.bin")
            .await
            .unwrap();
        first
            .brand(first.id(), "TEST", Some("First"))
            .await
            .unwrap();
        second
            .install_file(Box::new(Cursor::new(b"two")), "other.bin")
            .await
            .unwrap();
        second.brand(second.id(), "TEST", None).await.unwrap();

        let reopened = PlatformRepository::open(root.path().join("TEST.git"), "TEST").unwrap();
        assert_eq!(reopened.ids(StorageKind::Nand).unwrap().len(), 2);
        let first = reopened.volume(StorageKind::Nand, first.id());
        let mut bytes = Vec::new();
        first
            .open_read("deep/game/content.bin")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"one");
        assert_eq!(first.label().await.unwrap().as_deref(), Some("First"));
    }

    #[tokio::test]
    async fn nands_and_profiles_are_distinct_trees_in_one_native_checkout() {
        let root = tempfile::tempdir().unwrap();
        let platform = PlatformRepository::open(root.path().join("TEST.git"), "TEST").unwrap();
        let nand = platform.volume(StorageKind::Nand, Uuid::now_v7());
        let profile = platform.volume(StorageKind::SaveRepo, Uuid::now_v7());

        nand.install_file(Box::new(Cursor::new(b"nand")), "deep/content.bin")
            .await
            .unwrap();
        nand.brand(nand.id(), "TEST", None).await.unwrap();
        profile
            .install_file(Box::new(Cursor::new(b"profile")), "saves/game/save.bin")
            .await
            .unwrap();
        profile.brand(profile.id(), "TEST", None).await.unwrap();

        assert_ne!(nand.checkout_path(), profile.checkout_path());
        assert_eq!(nand.index_path(), profile.index_path());
        assert_eq!(
            fs::read(nand.checkout_path().join("deep/content.bin")).unwrap(),
            b"nand"
        );
        assert_eq!(
            fs::read(profile.checkout_path().join("saves/game/save.bin")).unwrap(),
            b"profile"
        );
        assert!(nand.index_path().is_file());
        assert_eq!(nand.index_path(), platform.path().join(".git/index"));

        let repo = platform.repository().to_thread_local();
        let index = gix::index::File::at(
            nand.index_path(),
            repo.object_hash(),
            false,
            gix::index::decode::Options::default(),
        )
        .unwrap();
        let paths = index
            .entries()
            .iter()
            .map(|entry| entry.path(&index).to_str_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            [
                format!("files/nands/{}/deep/content.bin", nand.id().simple()),
                format!(
                    "files/save-repos/{}/saves/game/save.bin",
                    profile.id().simple()
                ),
            ]
        );

        let handle = profile.open("saves/game/save.bin", true).unwrap();
        handle.set_len(0).unwrap();
        handle.write_all_at(0, b"updated").unwrap();
        profile.commit_pending("update profile", None).unwrap();
        drop(repo);

        let reopened = platform.volume(StorageKind::SaveRepo, profile.id());
        let mut contents = Vec::new();
        reopened
            .open_read("saves/game/save.bin")
            .unwrap()
            .read_to_end(&mut contents)
            .unwrap();
        assert_eq!(contents, b"updated");
    }

    #[tokio::test]
    async fn unchanged_checkout_writes_do_not_create_commits() {
        let root = tempfile::tempdir().unwrap();
        let platform = PlatformRepository::open(root.path().join("TEST.git"), "TEST").unwrap();
        let profile = platform.volume(StorageKind::SaveRepo, Uuid::now_v7());
        profile
            .install_file(Box::new(Cursor::new(b"unchanged")), "save/data.bin")
            .await
            .unwrap();
        profile.brand(profile.id(), "TEST", None).await.unwrap();

        let before = GitVolumeStorage::tip_in(
            &platform.repository().to_thread_local(),
            &profile.ref_name(),
        )
        .unwrap();
        profile
            .open("save/data.bin", true)
            .unwrap()
            .write_all_at(0, b"unchanged")
            .unwrap();

        assert_eq!(profile.commit_pending("no change", None).unwrap(), None);
        assert_eq!(
            GitVolumeStorage::tip_in(
                &platform.repository().to_thread_local(),
                &profile.ref_name(),
            )
            .unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn profile_forks_share_history_then_diverge_without_reaching_nand() {
        let root = tempfile::tempdir().unwrap();
        let platform = PlatformRepository::open(root.path().join("TEST.git"), "TEST").unwrap();
        let nand = platform.volume(StorageKind::Nand, Uuid::now_v7());
        nand.install_file(Box::new(Cursor::new(b"large base")), "content/game.bin")
            .await
            .unwrap();
        nand.brand(nand.id(), "TEST", None).await.unwrap();

        let source = platform.volume(StorageKind::MemoryCard, Uuid::now_v7());
        source
            .install_file(Box::new(Cursor::new(b"save one")), "card/save.bin")
            .await
            .unwrap();
        source
            .brand(source.id(), "TEST", Some("Source"))
            .await
            .unwrap();
        let destination_id = Uuid::now_v7();
        let fork = platform
            .fork_profile(
                StorageKind::MemoryCard,
                source.id(),
                destination_id,
                Some("Fork"),
            )
            .unwrap();
        let repo = platform.repository().to_thread_local();
        let nand_tip = GitVolumeStorage::tip_in(&repo, &nand.ref_name())
            .unwrap()
            .unwrap();
        let fork_tip = GitVolumeStorage::tip_in(&repo, &fork.ref_name())
            .unwrap()
            .unwrap();
        let mut current = Some(fork_tip);
        while let Some(id) = current {
            assert_ne!(id, nand_tip);
            current = repo
                .find_commit(id)
                .unwrap()
                .parent_ids()
                .next()
                .map(|id| id.detach());
        }
        drop(repo);

        source
            .install_file(Box::new(Cursor::new(b"save two")), "card/save.bin")
            .await
            .unwrap();
        source.commit_pending("source diverged", None).unwrap();
        let mut bytes = Vec::new();
        fork.open_read("card/save.bin")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"save one");
        assert_eq!(fork.label().await.unwrap().as_deref(), Some("Fork"));
        assert_eq!(platform.ids(StorageKind::MemoryCard).unwrap().len(), 2);

        platform
            .delete(StorageKind::MemoryCard, source.id())
            .unwrap();
        assert_eq!(
            platform.ids(StorageKind::MemoryCard).unwrap(),
            [destination_id]
        );
    }

    #[tokio::test]
    async fn uninstall_rebases_independent_later_depots() {
        let root = tempfile::tempdir().unwrap();
        let platform = PlatformRepository::open(root.path().join("TEST.git"), "TEST").unwrap();
        let nand = platform.volume(StorageKind::Nand, Uuid::now_v7());
        nand.brand(nand.id(), "TEST", None).await.unwrap();

        for (id, path, bytes) in [
            ("first", "first/content.bin", b"first".as_slice()),
            ("second", "second/content.bin", b"second".as_slice()),
        ] {
            let installed = nand
                .install_file(Box::new(Cursor::new(bytes)), path)
                .await
                .unwrap();
            nand.artifacts()
                .record(&Artifact::new(
                    id.to_owned(),
                    DepotType::App,
                    vec![DepotFile::new(
                        PathBuf::from(path),
                        installed.blake3,
                        installed.byte_length,
                    )],
                    id.to_owned(),
                    None,
                    IconHint::Builtin(DepotType::App),
                ))
                .await
                .unwrap();
        }

        let repo = platform.repository().to_thread_local();
        let old_tip = GitVolumeStorage::tip_in(&repo, &nand.ref_name())
            .unwrap()
            .unwrap();
        nand.artifacts().uninstall(&["first"]).await.unwrap();
        let new_tip = GitVolumeStorage::tip_in(&repo, &nand.ref_name())
            .unwrap()
            .unwrap();
        assert_ne!(new_tip, old_tip);
        assert!(!nand.exists("first/content.bin"));
        let mut bytes = Vec::new();
        nand.open_read("second/content.bin")
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"second");
        assert_eq!(
            nand.artifacts()
                .summaries()
                .await
                .unwrap()
                .into_iter()
                .map(|artifact| artifact.id)
                .collect::<Vec<_>>(),
            ["second"]
        );

        let mut current = Some(new_tip);
        let mut messages = Vec::new();
        while let Some(id) = current {
            let commit = repo.find_commit(id).unwrap();
            messages.push(commit.message_raw().unwrap().to_string());
            current = commit.parent_ids().next().map(|id| id.detach());
        }
        assert_eq!(
            messages,
            ["Polymer install second", "Polymer volume created"]
        );
    }
}
