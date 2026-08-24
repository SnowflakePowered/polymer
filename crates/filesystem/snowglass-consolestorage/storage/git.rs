use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use gix::bstr::{BStr, ByteSlice};
use gix::refs::Target;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit, RefLog};
use jiff::Timestamp;
use parking_lot::{Mutex, RwLock};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::installable::artifacts::Artifact;
use crate::installable::{DepotFile, DepotType, IconHint};
use crate::storage::manifest::{ArtifactRegistry, ArtifactSummary, VolumeInfo};
use crate::{
    BoxFuture, ChangeSignal, DirEntry, EntryKind, Error, InstallResult, Metadata, MountableStorage,
    ReadDir, Storage, StorageFile, VolumeStorage,
};

const SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitVolumeKind {
    Nand,
    SaveRepo,
    MemoryCard,
}

impl GitVolumeKind {
    fn ref_name(self, id: Uuid) -> String {
        let namespace = match self {
            Self::Nand => "nands",
            Self::SaveRepo | Self::MemoryCard => "profiles",
        };
        format!(
            "refs/snowglass/{namespace}/v{SCHEMA_VERSION}/{}",
            id.simple()
        )
    }

    fn metadata_name(self) -> &'static str {
        match self {
            Self::Nand => "nand",
            Self::SaveRepo => "save-repo",
            Self::MemoryCard => "memory-card",
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
        let repository = if path.exists() {
            gix::open(&path).map_err(|error| Error::git("open platform repository", error))?
        } else {
            std::fs::create_dir_all(
                path.parent()
                    .ok_or_else(|| Error::InvalidPath(path.display().to_string()))?,
            )
            .map_err(|error| Error::io(&path, error))?;
            gix::ThreadSafeRepository::init(
                &path,
                gix::create::Kind::Bare,
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
        let marker = "# snowglass platform storage v2";
        if !config.contains(marker) {
            config.push_str(&format!(
                "\n{marker}\n[snowglass]\n\tschemaVersion = {SCHEMA_VERSION}\n\tplatform = {platform}\n[core]\n\tlogAllRefUpdates = always\n"
            ));
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

    pub fn volume(&self, kind: GitVolumeKind, id: Uuid) -> GitVolumeStorage {
        GitVolumeStorage::new(self.clone(), kind, id)
    }

    pub fn contains(&self, kind: GitVolumeKind, id: Uuid) -> Result<bool, Error> {
        let repo = self.0.repository.to_thread_local();
        repo.try_find_reference(&kind.ref_name(id))
            .map(|reference| reference.is_some())
            .map_err(|error| Error::git("find volume reference", error))
    }

    pub fn ids(&self, kind: GitVolumeKind) -> Result<Vec<Uuid>, Error> {
        let repo = self.0.repository.to_thread_local();
        let namespace = match kind {
            GitVolumeKind::Nand => "nands",
            GitVolumeKind::SaveRepo | GitVolumeKind::MemoryCard => "profiles",
        };
        let prefix = format!("refs/snowglass/{namespace}/v{SCHEMA_VERSION}/");
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

    pub fn delete(&self, kind: GitVolumeKind, id: Uuid) -> Result<(), Error> {
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
        if kind != GitVolumeKind::Nand {
            let profile = id.simple().to_string();
            let prefixes = [
                format!("refs/snowglass/saves/v2/{profile}/"),
                format!("refs/notes/snowglass/v2/{profile}/"),
            ];
            let exact = [
                format!("refs/snowglass/archive/v2/{profile}/recently-deleted"),
                format!("refs/snowglass/transactions/v2/{profile}"),
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
            .map(|_| ())
            .map_err(|error| Error::git("delete volume reference", error))
    }

    pub fn fork_profile(
        &self,
        kind: GitVolumeKind,
        source: Uuid,
        destination: Uuid,
        label: Option<&str>,
    ) -> Result<GitVolumeStorage, Error> {
        if kind == GitVolumeKind::Nand {
            return Err(Error::Unsupported(
                "only save profiles and memory cards can be forked".to_owned(),
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
                "Snowglass profile fork",
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
                    message: "snowglass: fork profile".into(),
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
            let is_resource = name.starts_with(&format!("refs/snowglass/saves/v2/{source_id}/"))
                || name.starts_with(&format!("refs/notes/snowglass/v2/{source_id}/"));
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
                        message: "snowglass: fork profile resource".into(),
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

#[derive(Clone)]
enum OverlayEntry {
    File(GitFile),
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

struct GitVolumeInner {
    platform: PlatformRepository,
    kind: GitVolumeKind,
    id: Uuid,
    working: RwLock<WorkingState>,
    changed: Arc<Notify>,
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
    fn new(platform: PlatformRepository, kind: GitVolumeKind, id: Uuid) -> Self {
        let inner = Arc::new(GitVolumeInner {
            platform,
            kind,
            id,
            working: RwLock::new(WorkingState::default()),
            changed: Arc::new(Notify::new()),
        });
        Self {
            artifacts: GitArtifactRegistry(inner.clone()),
            inner,
        }
    }

    pub fn id(&self) -> Uuid {
        self.inner.id
    }

    pub fn kind(&self) -> GitVolumeKind {
        self.inner.kind
    }

    pub fn platform_repository(&self) -> &PlatformRepository {
        &self.inner.platform
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

    fn commit_time(repo: &gix::Repository, tip: Option<gix::ObjectId>) -> Timestamp {
        tip.and_then(|tip| repo.find_commit(tip).ok())
            .and_then(|commit| commit.time().ok())
            .and_then(|time| Timestamp::from_second(time.seconds).ok())
            .unwrap_or_else(Timestamp::now)
    }

    fn base_entry(&self, path: &str) -> Result<Option<BaseEntry>, Error> {
        let repo = self.inner.platform.repository().to_thread_local();
        let tip = Self::tip_in(&repo, &self.ref_name())?;
        let tree = Self::root_tree(&repo, tip)?;
        let git_path = if path.is_empty() {
            "files".to_owned()
        } else {
            format!("files/{path}")
        };
        let Some(entry) = tree
            .lookup_entry_by_path(&git_path)
            .map_err(|error| Error::git("lookup volume path", error))?
        else {
            if path.is_empty() || self.base_directories(&repo, &tree)?.contains(path) {
                return Ok(Some(BaseEntry {
                    kind: EntryKind::Directory,
                    id: None,
                    len: 0,
                    mtime: Self::commit_time(&repo, tip),
                }));
            }
            return Ok(None);
        };
        let kind = entry.mode().kind();
        let (kind, len) = match kind {
            gix::objs::tree::EntryKind::Tree => (EntryKind::Directory, 0),
            gix::objs::tree::EntryKind::Blob | gix::objs::tree::EntryKind::BlobExecutable => {
                let len = repo
                    .find_blob(entry.oid())
                    .map_err(|error| Error::git("read volume blob", error))?
                    .data
                    .len() as u64;
                (EntryKind::File, len)
            }
            _ => {
                return Err(Error::HistoryConflict(format!(
                    "unsupported Git entry at `{path}`"
                )));
            }
        };
        Ok(Some(BaseEntry {
            kind,
            id: Some(entry.object_id()),
            len,
            mtime: Self::commit_time(&repo, tip),
        }))
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

    fn base_file(&self, path: &str) -> Result<Vec<u8>, Error> {
        let entry = self
            .base_entry(path)?
            .ok_or_else(|| Error::not_found(path))?;
        if entry.kind != EntryKind::File {
            return Err(Error::not_found(path));
        }
        let repo = self.inner.platform.repository().to_thread_local();
        repo.find_blob(entry.id.expect("file has an object id"))
            .map(|blob| blob.data.clone())
            .map_err(|error| Error::git("read volume file", error))
    }

    fn hidden_by_delete(entries: &BTreeMap<String, OverlayEntry>, path: &str) -> bool {
        let mut current = path;
        loop {
            if matches!(entries.get(current), Some(OverlayEntry::Deleted)) {
                return true;
            }
            let Some((parent, _)) = current.rsplit_once('/') else {
                break;
            };
            current = parent;
        }
        false
    }

    fn overlay_metadata(entry: &OverlayEntry) -> Option<Metadata> {
        match entry {
            OverlayEntry::File(file) => Some(file.metadata()),
            OverlayEntry::Directory => Some(Metadata {
                kind: EntryKind::Directory,
                len: 0,
                mtime: Timestamp::now(),
            }),
            OverlayEntry::Deleted => None,
        }
    }

    fn notify(&self) {
        self.inner.changed.notify_waiters();
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
        if extra_parent.is_none()
            && state.entries.is_empty()
            && state.metadata.volume.is_none()
            && state.metadata.artifacts.is_empty()
            && state.metadata.save_tips.is_empty()
        {
            return Ok(None);
        }
        let root = Self::root_tree(&repo, parent)?;
        let mut editor = root
            .edit()
            .map_err(|error| Error::git("edit volume tree", error))?;

        for (path, entry) in &state.entries {
            let git_path = format!("files/{path}");
            match entry {
                OverlayEntry::File(file) => {
                    let id = repo
                        .write_blob(file.bytes())
                        .map_err(|error| Error::git("write volume blob", error))?
                        .detach();
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

struct BaseEntry {
    kind: EntryKind,
    id: Option<gix::ObjectId>,
    len: u64,
    mtime: Timestamp,
}

#[derive(Clone)]
struct GitFile(Arc<RwLock<GitFileState>>);

struct GitFileState {
    bytes: Vec<u8>,
    mtime: Timestamp,
}

impl GitFile {
    fn new(bytes: Vec<u8>) -> Self {
        Self(Arc::new(RwLock::new(GitFileState {
            bytes,
            mtime: Timestamp::now(),
        })))
    }

    fn bytes(&self) -> Vec<u8> {
        self.0.read().bytes.clone()
    }

    fn metadata(&self) -> Metadata {
        let state = self.0.read();
        Metadata {
            kind: EntryKind::File,
            len: state.bytes.len() as u64,
            mtime: state.mtime,
        }
    }
}

struct GitFileHandle {
    file: GitFile,
    writable: bool,
    changed: Arc<Notify>,
}

impl GitFileHandle {
    fn writable(&self) -> Result<(), Error> {
        self.writable
            .then_some(())
            .ok_or_else(|| Error::Unsupported("Git file handle is read-only".to_owned()))
    }
}

impl StorageFile for GitFileHandle {
    fn metadata(&self) -> Result<Metadata, Error> {
        Ok(self.file.metadata())
    }

    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, Error> {
        let state = self.file.0.read();
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(state.bytes.len());
        let copied = output.len().min(state.bytes.len() - start);
        output[..copied].copy_from_slice(&state.bytes[start..start + copied]);
        Ok(copied)
    }

    fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, Error> {
        self.writable()?;
        let start = usize::try_from(offset)
            .map_err(|_| Error::Unsupported("Git file offset is too large".to_owned()))?;
        let end = start
            .checked_add(input.len())
            .ok_or_else(|| Error::Unsupported("Git file write is too large".to_owned()))?;
        let mut state = self.file.0.write();
        if end > state.bytes.len() {
            state.bytes.resize(end, 0);
        }
        state.bytes[start..end].copy_from_slice(input);
        state.mtime = Timestamp::now();
        drop(state);
        self.changed.notify_waiters();
        Ok(input.len())
    }

    fn set_len(&self, len: u64) -> Result<(), Error> {
        self.writable()?;
        let len = usize::try_from(len)
            .map_err(|_| Error::Unsupported("Git file is too large".to_owned()))?;
        let mut state = self.file.0.write();
        state.bytes.resize(len, 0);
        state.mtime = Timestamp::now();
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }

    fn flush(&self) -> Result<(), Error> {
        Ok(())
    }

    fn sync(&self) -> Result<(), Error> {
        Ok(())
    }
}

impl Storage for GitVolumeStorage {
    fn exists(&self, path: &str) -> bool {
        self.metadata(path).is_ok()
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        Self::validate(path)?;
        let working = self.inner.working.read();
        if let Some(entry) = working.entries.get(path) {
            return Self::overlay_metadata(entry).ok_or_else(|| Error::not_found(path));
        }
        if Self::hidden_by_delete(&working.entries, path) {
            return Err(Error::not_found(path));
        }
        drop(working);
        let entry = self
            .base_entry(path)?
            .ok_or_else(|| Error::not_found(path))?;
        Ok(Metadata {
            kind: entry.kind,
            len: entry.len,
            mtime: entry.mtime,
        })
    }

    fn read_dir(&self, path: &str, recursive: bool) -> Result<ReadDir<'_>, Error> {
        Self::validate(path)?;
        if recursive {
            return ReadDir::recursive(self, path);
        }
        if self.metadata(path)?.kind != EntryKind::Directory {
            return Err(Error::not_found(path));
        }
        let repo = self.inner.platform.repository().to_thread_local();
        let tip = Self::tip_in(&repo, &self.ref_name())?;
        let root = Self::root_tree(&repo, tip)?;
        let git_path = if path.is_empty() {
            "files".to_owned()
        } else {
            format!("files/{path}")
        };
        let mut entries = BTreeMap::<String, DirEntry>::new();
        if let Some(entry) = root
            .lookup_entry_by_path(&git_path)
            .map_err(|error| Error::git("lookup volume directory", error))?
            && entry.mode().kind() == gix::objs::tree::EntryKind::Tree
        {
            let tree = repo
                .find_tree(entry.oid())
                .map_err(|error| Error::git("read volume directory", error))?;
            for child in tree.iter() {
                let child = child.map_err(|error| Error::git("decode volume directory", error))?;
                let name = child.filename().to_str_lossy().into_owned();
                let child_path = join(path, &name);
                let kind = match child.kind() {
                    gix::objs::tree::EntryKind::Tree => EntryKind::Directory,
                    gix::objs::tree::EntryKind::Blob
                    | gix::objs::tree::EntryKind::BlobExecutable => EntryKind::File,
                    _ => continue,
                };
                let len = if kind == EntryKind::File {
                    repo.find_blob(child.oid())
                        .map_err(|error| Error::git("read volume directory blob", error))?
                        .data
                        .len() as u64
                } else {
                    0
                };
                entries.insert(
                    name.clone(),
                    DirEntry {
                        name,
                        path: child_path,
                        metadata: Metadata {
                            kind,
                            len,
                            mtime: Self::commit_time(&repo, tip),
                        },
                    },
                );
            }
        }
        for directory in self.base_directories(&repo, &root)? {
            if let Some(name) = immediate_child(path, &directory) {
                let child_path = join(path, name);
                entries.entry(name.to_owned()).or_insert(DirEntry {
                    name: name.to_owned(),
                    path: child_path,
                    metadata: Metadata {
                        kind: EntryKind::Directory,
                        len: 0,
                        mtime: Self::commit_time(&repo, tip),
                    },
                });
            }
        }
        let working = self.inner.working.read();
        for (candidate, entry) in &working.entries {
            let Some(name) = immediate_child(path, candidate) else {
                continue;
            };
            let child_path = join(path, name);
            if candidate == &child_path && matches!(entry, OverlayEntry::Deleted) {
                entries.remove(name);
                continue;
            }
            let metadata = if candidate == &child_path {
                Self::overlay_metadata(entry)
            } else if !matches!(entry, OverlayEntry::Deleted) {
                Some(Metadata {
                    kind: EntryKind::Directory,
                    len: 0,
                    mtime: Timestamp::now(),
                })
            } else {
                None
            };
            if let Some(metadata) = metadata {
                entries.insert(
                    name.to_owned(),
                    DirEntry {
                        name: name.to_owned(),
                        path: child_path,
                        metadata,
                    },
                );
            }
        }
        Ok(ReadDir::surface(entries.into_values()))
    }

    fn create_dir_all(&self, path: &str) -> Result<(), Error> {
        Self::validate(path)?;
        if path.is_empty() {
            return Ok(());
        }
        let mut current = String::new();
        let mut working = self.inner.working.write();
        for component in path.split('/') {
            current = join(&current, component);
            if working
                .entries
                .get(&current)
                .is_some_and(|entry| matches!(entry, OverlayEntry::File(_)))
                || (!working.entries.contains_key(&current)
                    && self
                        .base_entry(&current)?
                        .is_some_and(|entry| entry.kind == EntryKind::File))
            {
                return Err(Error::Unsupported(format!(
                    "file `{current}` cannot be used as a directory"
                )));
            }
            working
                .entries
                .insert(current.clone(), OverlayEntry::Directory);
        }
        drop(working);
        self.notify();
        Ok(())
    }

    fn create(&self, path: &str) -> Result<Arc<dyn StorageFile>, Error> {
        Self::validate(path)?;
        if path.is_empty() {
            return Err(Error::InvalidPath(path.to_owned()));
        }
        if let Some((parent, _)) = path.rsplit_once('/') {
            self.create_dir_all(parent)?;
        }
        let file = GitFile::new(Vec::new());
        self.inner
            .working
            .write()
            .entries
            .insert(path.to_owned(), OverlayEntry::File(file.clone()));
        self.notify();
        Ok(Arc::new(GitFileHandle {
            file,
            writable: true,
            changed: self.inner.changed.clone(),
        }))
    }

    fn open(&self, path: &str, write: bool) -> Result<Arc<dyn StorageFile>, Error> {
        Self::validate(path)?;
        let file = {
            let working = self.inner.working.read();
            match working.entries.get(path) {
                Some(OverlayEntry::File(file)) => Some(file.clone()),
                Some(OverlayEntry::Directory | OverlayEntry::Deleted) => {
                    return Err(Error::not_found(path));
                }
                None if Self::hidden_by_delete(&working.entries, path) => {
                    return Err(Error::not_found(path));
                }
                None => None,
            }
        };
        let file = match file {
            Some(file) => file,
            None => {
                let file = GitFile::new(self.base_file(path)?);
                if write {
                    self.inner
                        .working
                        .write()
                        .entries
                        .insert(path.to_owned(), OverlayEntry::File(file.clone()));
                }
                file
            }
        };
        Ok(Arc::new(GitFileHandle {
            file,
            writable: write,
            changed: self.inner.changed.clone(),
        }))
    }

    fn remove(&self, path: &str) -> Result<(), Error> {
        Self::validate(path)?;
        if path.is_empty() {
            return Err(Error::InvalidPath(path.to_owned()));
        }
        if self.exists(path)
            && self.metadata(path)?.kind == EntryKind::Directory
            && self.read_dir(path, false)?.next().is_some()
        {
            return Err(Error::Unsupported(format!(
                "directory `{path}` is not empty"
            )));
        }
        self.inner
            .working
            .write()
            .entries
            .insert(path.to_owned(), OverlayEntry::Deleted);
        self.notify();
        Ok(())
    }

    fn watch(&self, _paths: &[&str]) -> Result<Option<Box<dyn ChangeSignal>>, Error> {
        Ok(Some(Box::new(GitChangeSignal(self.inner.changed.clone()))))
    }

    fn install_file<'a>(
        &'a self,
        mut source: Box<dyn AsyncRead + Send + Unpin>,
        dest: &str,
    ) -> BoxFuture<'a, Result<InstallResult, Error>> {
        let dest = dest.to_owned();
        Box::pin(async move {
            Self::validate(&dest)?;
            let mut bytes = Vec::new();
            source
                .read_to_end(&mut bytes)
                .await
                .map_err(|error| Error::Io {
                    path: dest.clone(),
                    source: error,
                })?;
            let result = InstallResult {
                blake3: *blake3::hash(&bytes).as_bytes(),
                byte_length: bytes.len() as u64,
            };
            if let Some((parent, _)) = dest.rsplit_once('/') {
                self.create_dir_all(parent)?;
            }
            self.inner
                .working
                .write()
                .entries
                .insert(dest, OverlayEntry::File(GitFile::new(bytes)));
            self.notify();
            Ok(result)
        })
    }
}

impl MountableStorage for GitVolumeStorage {
    fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        Self::validate(from)?;
        Self::validate(to)?;
        let metadata = self.metadata(from)?;
        if metadata.kind == EntryKind::File {
            let file = {
                let working = self.inner.working.write();
                match working.entries.get(from) {
                    Some(OverlayEntry::File(file)) => file.clone(),
                    _ => GitFile::new(self.base_file(from)?),
                }
            };
            if let Some((parent, _)) = to.rsplit_once('/') {
                self.create_dir_all(parent)?;
            }
            let mut working = self.inner.working.write();
            working
                .entries
                .insert(from.to_owned(), OverlayEntry::Deleted);
            working
                .entries
                .insert(to.to_owned(), OverlayEntry::File(file));
        } else {
            let entries = self.read_dir(from, true)?.collect::<Vec<_>>();
            self.create_dir_all(to)?;
            for entry in entries {
                let relative = entry
                    .path
                    .strip_prefix(from)
                    .unwrap()
                    .trim_start_matches('/');
                let target = join(to, relative);
                match entry.metadata.kind {
                    EntryKind::Directory => self.create_dir_all(&target)?,
                    EntryKind::File => {
                        let file = self.open(&entry.path, false)?;
                        let mut bytes = vec![0; entry.metadata.len as usize];
                        file.read_at(0, &mut bytes)?;
                        self.inner
                            .working
                            .write()
                            .entries
                            .insert(target, OverlayEntry::File(GitFile::new(bytes)));
                    }
                }
            }
            self.inner
                .working
                .write()
                .entries
                .insert(from.to_owned(), OverlayEntry::Deleted);
        }
        self.notify();
        Ok(())
    }

    fn truncate(&self, path: &str, len: u64) -> Result<(), Error> {
        self.open(path, true)?.set_len(len)
    }

    fn sync(&self, _path: &str) -> Result<(), Error> {
        Ok(())
    }
}

impl VolumeStorage for GitVolumeStorage {
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
            self.commit_pending("Snowglass volume label changed", None)?;
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
            self.commit_pending("Snowglass volume created", None)?;
            Ok(info)
        })
    }
}

struct GitChangeSignal(Arc<Notify>);

impl ChangeSignal for GitChangeSignal {
    fn changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.0.notified())
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
    byte_length: u64,
}

#[derive(Clone)]
struct ArtifactFileRecord {
    path: PathBuf,
    blake3: [u8; 32],
    byte_length: u64,
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
                IconHint::Builtin(_) => None,
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
            self.icon_path
                .clone()
                .map(IconHint::File)
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
}

impl ArtifactRegistry for GitArtifactRegistry {
    fn record<'a>(&'a self, artifact: &'a Artifact) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            self.0.working.write().metadata.artifacts.insert(
                artifact.id().to_owned(),
                Some(ArtifactRecord::from(artifact)),
            );
            self.storage()
                .commit_pending(&format!("Snowglass install {}", artifact.id()), None)?;
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

    fn forget<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            self.0
                .working
                .write()
                .metadata
                .artifacts
                .insert(id.to_owned(), None);
            self.storage()
                .commit_pending(&format!("Snowglass uninstall {id}"), None)?;
            Ok(())
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
        name: BStr::new(b"Snowglass"),
        email: BStr::new(b"storage@snowglass.invalid"),
        time: &time,
    }
    .to_owned()
    .expect("Snowglass generates a valid Git signature")
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

fn immediate_child<'a>(parent: &str, candidate: &'a str) -> Option<&'a str> {
    let relative = if parent.is_empty() {
        candidate
    } else {
        candidate.strip_prefix(parent)?.strip_prefix('/')?
    };
    let child = relative.split('/').next()?;
    (!child.is_empty()).then_some(child)
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
        let first = platform.volume(GitVolumeKind::Nand, Uuid::now_v7());
        let second = platform.volume(GitVolumeKind::Nand, Uuid::now_v7());
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
        assert_eq!(reopened.ids(GitVolumeKind::Nand).unwrap().len(), 2);
        let first = reopened.volume(GitVolumeKind::Nand, first.id());
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
    async fn profile_forks_share_history_then_diverge_without_reaching_nand() {
        let root = tempfile::tempdir().unwrap();
        let platform = PlatformRepository::open(root.path().join("TEST.git"), "TEST").unwrap();
        let nand = platform.volume(GitVolumeKind::Nand, Uuid::now_v7());
        nand.install_file(Box::new(Cursor::new(b"large base")), "content/game.bin")
            .await
            .unwrap();
        nand.brand(nand.id(), "TEST", None).await.unwrap();

        let source = platform.volume(GitVolumeKind::MemoryCard, Uuid::now_v7());
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
                GitVolumeKind::MemoryCard,
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
        assert_eq!(platform.ids(GitVolumeKind::MemoryCard).unwrap().len(), 2);

        platform
            .delete(GitVolumeKind::MemoryCard, source.id())
            .unwrap();
        assert_eq!(
            platform.ids(GitVolumeKind::MemoryCard).unwrap(),
            [destination_id]
        );
    }
}
